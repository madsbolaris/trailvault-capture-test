use crate::{
    VERSION,
    git::{self, Repo},
    redact::{self, Redactor},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use nix::poll::{PollFd, PollFlags, poll};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::{fd::AsFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use ulid::Ulid;

type Snapshot = BTreeMap<String, Option<String>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub session_id: String,
    pub agent_type: String,
    pub worktree_path: PathBuf,
    pub base_commit: String,
    pub branch: String,
    pub started_at: String,
    pub phase: String,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub touched_file_hashes: Snapshot,
    #[serde(default)]
    pub checkpoint_count: u64,
    #[serde(default)]
    pub step_count: u64,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub baseline: Snapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_transcript: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checkpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: BTreeMap<String, crate::subagent::Task>,
}
#[derive(Serialize, Deserialize)]
struct PlanSession {
    id: String,
    transcript_hash: String,
    files: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Plan {
    checkpoint: String,
    tree: String,
    sessions: Vec<PlanSession>,
}
struct Entry {
    state: State,
    full: String,
    touched: Vec<String>,
}

pub fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn checkpoint_id() -> String {
    Ulid::new().to_string()
}
pub fn checkpoint_ref(id: &str) -> Result<String> {
    ensure!(
        id.len() == 26
            && id
                .bytes()
                .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b)),
        "Invalid checkpoint ID: {id}"
    );
    Ok(format!("refs/trailvault/checkpoints/{}/{id}", &id[24..]))
}
pub fn trailer(message: &str, name: &str) -> Option<String> {
    message.lines().find_map(|line| {
        let id = line.strip_prefix(&format!("{name}: "))?;
        checkpoint_ref(id).ok().map(|_| id.to_owned())
    })
}
pub fn merge(left: &mut Value, right: &Value) -> Result<()> {
    let target = left.as_object_mut().context("Settings must be an object")?;
    for (key, value) in right.as_object().context("Settings must be an object")? {
        ensure!(
            !["__proto__", "constructor", "prototype"].contains(&key.as_str()),
            "Invalid settings key {key}"
        );
        if value.is_object() {
            let item = target.entry(key.clone()).or_insert(json!({}));
            if !item.is_object() {
                *item = json!({});
            }
            merge(item, value)?;
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}
pub fn settings(repo: &Repo) -> Result<Value> {
    settings_with_defaults(repo, json!({"enabled":false, "commit_linking":"prompt"}))
}
pub fn settings_with_defaults(repo: &Repo, mut result: Value) -> Result<Value> {
    for name in ["settings.json", "settings.local.json"] {
        if let Some(value) = git::optional::<Value>(&repo.root.join(".trailvault").join(name))? {
            merge(&mut result, &value)?;
        }
    }
    ensure!(
        matches!(result["commit_linking"].as_str(), Some("prompt" | "always")),
        "commit_linking must be prompt or always"
    );
    ensure!(result["enabled"].is_boolean(), "enabled must be boolean");
    if let Some(value) = result.pointer("/strategy_options/push_sessions") {
        ensure!(
            value.is_boolean(),
            "strategy_options.push_sessions must be boolean"
        );
    }
    if let Some(value) = result.get("checkpoint_push_remote") {
        ensure!(
            value.is_string(),
            "checkpoint_push_remote must be a remote name or URL"
        );
    }
    Redactor::new(&result)?;
    Ok(result)
}
fn state_path(repo: &Repo, id: &str) -> Result<PathBuf> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Invalid session ID"
    );
    Ok(repo
        .common
        .join("trailvault-sessions")
        .join(format!("{id}.json")))
}
pub fn sessions(repo: &Repo) -> Result<Vec<State>> {
    let directory = repo.common.join("trailvault-sessions");
    let mut states = Vec::new();
    if !directory.try_exists()? {
        return Ok(states);
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_some_and(|s| s == "json") {
            let state: State = git::read(&path)?;
            if state.worktree_path == repo.root {
                states.push(state);
            }
        }
    }
    states.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Ok(states)
}
fn queue_path(repo: &Repo) -> PathBuf {
    repo.common.join("trailvault/push-queue.json")
}
fn plan_path(repo: &Repo) -> PathBuf {
    repo.git_dir.join("trailvault-commit.json")
}
fn hook_location(repo: &Repo, name: &str) -> Result<PathBuf> {
    Ok(repo
        .root
        .join(repo.g(&["rev-parse", "--git-path", &format!("hooks/{name}")])?))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn shell_path(path: &Path) -> Result<String> {
    Ok(quote(
        path.to_str().context("Hook path must be valid UTF-8")?,
    ))
}

fn validate_git_hook_paths(repo: &Repo) -> Result<()> {
    for name in ["prepare-commit-msg", "post-commit", "pre-push"] {
        let path = hook_location(repo, name)?;
        let directory = path.parent().context("Hook has no parent")?;
        let resolved = if directory.try_exists()? {
            directory.canonicalize()?
        } else {
            directory.to_owned()
        };
        ensure!(
            resolved.starts_with(&repo.root) || resolved.starts_with(&repo.common),
            "Refusing to modify a shared external hooks directory; configure a repository-local core.hooksPath first"
        );
    }
    Ok(())
}
fn write_git_hooks(repo: &Repo, executable: &Path) -> Result<()> {
    validate_git_hook_paths(repo)?;
    for name in ["prepare-commit-msg", "post-commit", "pre-push"] {
        let path = hook_location(repo, name)?;
        let backup = PathBuf::from(format!("{}.pre-trailvault", path.display()));
        if path.try_exists()? && !fs::read_to_string(&path)?.contains("# trailvault-managed-v1") {
            ensure!(
                !backup.try_exists()?,
                "Hook backup already exists: {}",
                backup.display()
            );
            fs::rename(&path, &backup)?;
        }
        let backup = shell_path(&backup)?;
        let body = format!(
            "#!/bin/sh\n# trailvault-managed-v1\n{} hooks git {name} \"$@\"\nstatus=$?\n[ \"$status\" -eq 0 ] || exit \"$status\"\nif [ -x {backup} ]; then\n  exec {backup} \"$@\"\nfi\n",
            shell_path(executable)?
        );
        git::atomic(&path, body.as_bytes())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
pub fn refresh_git_hooks(repo: &Repo, executable: &Path) -> Result<()> {
    if settings(repo)?["enabled"] == false || !crate::policy::capture_allowed(repo)? {
        return Ok(());
    }
    crate::install::validate_repository_paths(repo)?;
    let _lock = repo.lock()?;
    write_git_hooks(repo, executable)
}
pub fn enable(repo: &Repo, executable: &Path) -> Result<()> {
    let global = crate::install::installed()?;
    enable_options(repo, executable, global, false)
}
pub fn enable_local(repo: &Repo, executable: &Path) -> Result<()> {
    enable_options(repo, executable, true, true)
}
pub fn enable_for_policy(repo: &Repo, executable: &Path) -> Result<()> {
    enable_options(repo, executable, true, false)
}
fn enable_options(repo: &Repo, executable: &Path, global: bool, automatic: bool) -> Result<()> {
    validate_git_hook_paths(repo)?;
    let _lock = repo.lock()?;
    if automatic
        && settings_with_defaults(repo, json!({"enabled":true,"commit_linking":"prompt"}))?["enabled"]
            == false
    {
        return Ok(());
    }
    if global {
        crate::install::remove_repository_registration(repo)?;
    }
    let previous = settings(repo)?;
    let path = repo.root.join(if global {
        ".trailvault/settings.local.json"
    } else {
        ".trailvault/settings.json"
    });
    let mut shared = git::optional::<Value>(&path)?.unwrap_or(json!({}));
    merge(
        &mut shared,
        &json!({"enabled":true,"telemetry":false,"checkpoints":{"primary":{"type":"git-refs"}}}),
    )?;
    if global
        && previous
            .pointer("/strategy_options/push_sessions")
            .is_none()
    {
        merge(
            &mut shared,
            &json!({"strategy_options":{"push_sessions":false}}),
        )?;
    }
    git::save(&path, &shared)?;
    let local_path = repo.root.join(".trailvault/settings.local.json");
    if let Some(mut local) = git::optional::<Value>(&local_path)? {
        if local["enabled"] == false {
            local["enabled"] = json!(true);
            git::save(&local_path, &local)?;
        }
    }
    let exclude = repo
        .root
        .join(repo.g(&["rev-parse", "--git-path", "info/exclude"])?);
    let mut ignore = match fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    for rule in [
        ".trailvault/metadata/",
        ".trailvault/tmp/",
        ".trailvault/settings.local.json",
    ] {
        if !ignore.lines().any(|line| line == rule) {
            if !ignore.ends_with('\n') {
                ignore.push('\n');
            }
            ignore.push_str(rule);
            ignore.push('\n');
        }
    }
    git::atomic(&exclude, ignore.as_bytes())?;
    write_git_hooks(repo, executable)?;
    if !global {
        git::save(
            &repo.root.join(".github/hooks/trailvault.json"),
            &crate::install::configuration(executable, false)?,
        )?;
    }
    Ok(())
}
fn snapshot(repo: &Repo) -> Result<Snapshot> {
    let paths = repo.g(&[
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ])?;
    let mut result = BTreeMap::new();
    for path in paths
        .split('\0')
        .filter(|p| !p.is_empty() && !p.starts_with(".trailvault/"))
    {
        let metadata = match fs::symlink_metadata(repo.root.join(path)) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let hash = if metadata.is_some_and(|m| m.is_file() && !m.is_symlink()) {
            Some(repo.g(&["hash-object", "--", path])?)
        } else {
            None
        };
        result.insert(path.to_owned(), hash);
    }
    Ok(result)
}
fn transcript_path(input: &Value, id: &str) -> Result<PathBuf> {
    let home = env::var_os("COPILOT_HOME").map(PathBuf::from).unwrap_or(
        env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is unset")?
            .join(".copilot"),
    );
    let allowed = home.join("session-state").canonicalize()?;
    let path = input
        .get("transcriptPath")
        .map(|v| {
            v.as_str()
                .context("transcriptPath must be a string")
                .map(PathBuf::from)
        })
        .transpose()?
        .unwrap_or_else(|| allowed.join(id).join("events.jsonl"));
    let resolved = path
        .canonicalize()
        .with_context(|| format!("Cannot open transcript {}", path.display()))?;
    ensure!(
        resolved.starts_with(&allowed),
        "Transcript must be inside {}",
        allowed.display()
    );
    Ok(resolved)
}
pub fn agent_hook(repo: &Repo, name: &str, input: &Value) -> Result<()> {
    if !crate::policy::capture_allowed(repo)? {
        eprintln!("trailvault: repository policy has no current capture consent; capture skipped.");
        return Ok(());
    }
    crate::install::validate_repository_paths(repo)?;
    let config = settings(repo)?;
    if config["enabled"] == false {
        return Ok(());
    }
    if ["pre-tool-use", "post-tool-use"].contains(&name)
        && config.pointer("/strategy_options/publish_on_tool_completion") != Some(&json!(true))
    {
        return Ok(());
    }
    if name == "error-occurred" {
        eprintln!(
            "trailvault: Copilot reported an error: {}",
            input
                .get("error")
                .or_else(|| input.get("message"))
                .unwrap_or(&json!("unspecified"))
        );
        return Ok(());
    }
    ensure!(
        crate::install::EVENTS
            .iter()
            .any(|(_, command)| *command == name),
        "Unsupported Copilot hook {name}"
    );
    let id = input["sessionId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("sessionId is required")?;
    let path = state_path(repo, id)?;
    if name == "post-tool-use" {
        // Hosted commits can run prepare-commit-msg without the finalization hooks.
        // Recover the prepared bytes before replacing them with a newer snapshot.
        post_commit(repo)?;
        if settings(repo)?.pointer("/strategy_options/publish_on_tool_completion")
            == Some(&json!(true))
            && settings(repo)?.pointer("/strategy_options/cloud_artifact") != Some(&json!(true))
        {
            push(repo, "origin", true)?;
        }
    }
    let _lock = repo.lock()?;
    let existing: Option<State> = git::optional(&path)?;
    ensure!(
        name != "subagent-stop" || existing.is_some(),
        "Subagent stop requires an existing parent session"
    );
    let mut state = match existing {
        Some(state) => state,
        None => State {
            session_id: id.into(),
            agent_type: "Copilot CLI".into(),
            worktree_path: repo.root.clone(),
            base_commit: repo.g(&["rev-parse", "HEAD"])?,
            branch: git::run(
                &repo.root,
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                &BTreeMap::new(),
                None,
            )?
            .stdout
            .trim_end()
            .into(),
            started_at: Utc::now().to_rfc3339(),
            phase: "active".into(),
            files_touched: vec![],
            touched_file_hashes: BTreeMap::new(),
            checkpoint_count: 0,
            step_count: 0,
            prompts: vec![],
            baseline: snapshot(repo)?,
            last_prompt: None,
            transcript_path: None,
            transcript_hash: None,
            pending_transcript: None,
            ended_at: None,
            last_checkpoint_id: None,
            tasks: BTreeMap::new(),
        },
    };
    if name == "subagent-stop" {
        let transcript = transcript_path(input, id)?;
        let expected = crate::install::home()?
            .join("session-state")
            .join(id)
            .join("events.jsonl")
            .canonicalize()?;
        ensure!(
            transcript == expected,
            "Subagent evidence must use this parent session's native transcript"
        );
        let events = redact::parse(&fs::read_to_string(transcript)?)?;
        let task = crate::subagent::from_events(input, &events)?;
        if let Some(existing) = state.tasks.get(&task.tool_use_id) {
            ensure!(
                existing.agent_id == task.agent_id,
                "Task tool ID already belongs to a different child"
            );
        } else {
            state.tasks.insert(task.tool_use_id.clone(), task);
        }
        return git::save(&path, &state);
    }
    if name == "user-prompt-submitted" {
        let prompt = input["prompt"]
            .as_str()
            .context("prompt must be a string")?;
        state.baseline = snapshot(repo)?;
        state.prompts.push(prompt.into());
        state.last_prompt = Some(prompt.into());
        state.phase = "active".into();
    }
    if ["pre-tool-use", "post-tool-use", "agent-stop", "session-end"].contains(&name) {
        let transcript = transcript_path(input, id)?;
        let content = fs::read_to_string(&transcript)?;
        redact::parse(&content)?;
        let after = snapshot(repo)?;
        let paths: BTreeSet<_> = state.baseline.keys().chain(after.keys()).collect();
        for path in paths {
            if state.baseline.get(path) != after.get(path) {
                if !state.files_touched.contains(path) {
                    state.files_touched.push(path.clone());
                }
                state
                    .touched_file_hashes
                    .insert(path.clone(), after.get(path).cloned().flatten());
            }
        }
        state.baseline = after;
        if ["agent-stop", "session-end"].contains(&name) {
            state.step_count += 1;
        }
        state.transcript_path = Some(transcript);
        state.transcript_hash = Some(hash(&content));
        let pending = repo
            .root
            .join(".trailvault/metadata")
            .join(id)
            .join("full.jsonl");
        git::atomic(&pending, content.as_bytes())?;
        git::atomic(
            &pending.with_file_name("prompt.txt"),
            (state.prompts.join("\n\n") + "\n").as_bytes(),
        )?;
        state.pending_transcript = Some(pending);
        if name == "session-end" {
            state.phase = "ended".into();
            state.ended_at = Some(Utc::now().to_rfc3339());
        }
    }
    git::save(&path, &state)
}
fn changed_paths(repo: &Repo) -> Result<Vec<String>> {
    Ok(repo
        .g(&[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            "HEAD",
            "--",
        ])?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}
fn pending(repo: &Repo, paths: &[String]) -> Result<Vec<State>> {
    Ok(sessions(repo)?
        .into_iter()
        .filter(|s| {
            s.pending_transcript.is_some() && s.files_touched.iter().any(|p| paths.contains(p))
        })
        .collect())
}
#[derive(PartialEq)]
enum Choice {
    Yes,
    No,
    Always,
}
fn ask_link(prompt: &str) -> Result<Choice> {
    let mut terminal = match OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Ok(file) => file,
        Err(error)
            if error.raw_os_error().is_some_and(|e| {
                [
                    nix::libc::ENXIO,
                    nix::libc::ENOENT,
                    nix::libc::ENODEV,
                    nix::libc::ENOTTY,
                ]
                .contains(&e)
            }) =>
        {
            return Ok(Choice::Yes);
        }
        Err(error) => return Err(error.into()),
    };
    write!(
        terminal,
        "\nTrailvault: Active Copilot CLI session detected\n  Last prompt: {}\n\nLink this commit to session context?\n  [Y]es / [n]o / [a]lways: ",
        prompt.chars().take(80).collect::<String>()
    )?;
    terminal.flush()?;
    let mut descriptors = [PollFd::new(terminal.as_fd(), PollFlags::POLLIN)];
    if poll(&mut descriptors, 30_000_u16)? == 0 {
        writeln!(
            terminal,
            "\nNo answer within 30 seconds; linking by default."
        )?;
        return Ok(Choice::Yes);
    }
    let mut answer = String::new();
    BufReader::new(&terminal).read_line(&mut answer)?;
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "n" | "no" => Choice::No,
        "a" | "always" => Choice::Always,
        _ => Choice::Yes,
    })
}
pub fn prepare_commit(repo: &Repo, path: &Path, source: Option<&str>) -> Result<()> {
    crate::policy::enforce_write(repo)?;
    if !crate::policy::capture_allowed(repo)? {
        eprintln!("trailvault: checkpoint linking skipped without current policy consent.");
        return Ok(());
    }
    let config = settings(repo)?;
    if config["enabled"] == false || matches!(source, Some("merge" | "squash")) {
        return Ok(());
    }
    let message = fs::read_to_string(path)?;
    let candidates = {
        let _lock = repo.lock()?;
        git::remove(&plan_path(repo))?;
        pending(repo, &changed_paths(repo)?)?
    };
    if candidates.is_empty() {
        return Ok(());
    }
    let checkpoint = trailer(&message, "Trailvault-Checkpoint").unwrap_or_else(checkpoint_id);
    let choice = if source == Some("message")
        && config["commit_linking"] != "always"
        && env::var("COPILOT_AGENT_SESSION_ID")
            .unwrap_or_default()
            .is_empty()
    {
        ask_link(candidates[0].last_prompt.as_deref().unwrap_or(""))?
    } else {
        Choice::Yes
    };
    if choice == Choice::No {
        return Ok(());
    }
    let _lock = repo.lock()?;
    if choice == Choice::Always {
        update_local(repo, &json!({"commit_linking":"always"}))?;
    }
    let paths = changed_paths(repo)?;
    let states = pending(repo, &paths)?;
    if states.is_empty() {
        return Ok(());
    }
    let plan = Plan {
        checkpoint: checkpoint.clone(),
        tree: repo.g(&["write-tree"])?,
        sessions: states
            .into_iter()
            .map(|state| {
                Ok(PlanSession {
                    id: state.session_id,
                    transcript_hash: state
                        .transcript_hash
                        .context("Missing captured transcript hash")?,
                    files: state
                        .files_touched
                        .into_iter()
                        .filter(|p| paths.contains(p))
                        .collect(),
                })
            })
            .collect::<Result<_>>()?,
    };
    git::save(&plan_path(repo), &plan)?;
    if trailer(&message, "Trailvault-Checkpoint").is_none() {
        let lines: Vec<_> = message.split('\n').collect();
        let index = lines
            .iter()
            .position(|line| line.starts_with('#'))
            .unwrap_or(lines.len());
        let body = lines[..index].join("\n");
        let comments = lines[index..].join("\n");
        let comment = if source == Some("message") {
            ""
        } else {
            "# Remove the Trailvault-Checkpoint trailer above to decline session linking.\n"
        };
        git::atomic(
            path,
            format!(
                "{}\n\nTrailvault-Checkpoint: {checkpoint}\n{comment}{comments}",
                body.trim_end()
            )
            .as_bytes(),
        )?;
    }
    Ok(())
}
fn build_checkpoint(repo: &Repo, checkpoint: &str, entries: &[Entry]) -> Result<String> {
    let reference = checkpoint_ref(checkpoint)?;
    let old = git::run(
        &repo.root,
        &["rev-parse", "--verify", &reference],
        &BTreeMap::new(),
        None,
    )?;
    let directory = repo.common.join("trailvault");
    fs::create_dir_all(&directory)?;
    let temporary = TempDir::new_in(&directory)?;
    let index = temporary.path().join("index");
    let env = BTreeMap::from([(
        "GIT_INDEX_FILE".into(),
        index.to_str().context("Index path must be UTF-8")?.into(),
    )]);
    let mut files = BTreeMap::new();
    let mut metadata = vec![];
    let config = settings(repo)?;
    for (number, entry) in entries.iter().enumerate() {
        let mut redactor = Redactor::new(&config)?;
        let events: Vec<_> = redact::parse(&entry.full)?
            .iter()
            .map(|event| redactor.event(event))
            .collect();
        let full = redact::jsonl(&events)?;
        let compact = redact::compact(&events)?;
        for task in entry.state.tasks.values() {
            let mut value = serde_json::to_value(task)?;
            value["task_description"] = json!(redactor.text(&task.task_description));
            let task_path = format!("tasks/{}/task.json", task.tool_use_id);
            ensure!(
                !files.contains_key(&task_path),
                "Conflicting subagent tool-call IDs across sessions"
            );
            files.insert(task_path, serde_json::to_string_pretty(&value)? + "\n");
        }
        let prompt = redactor.text(&entry.state.prompts.join("\n\n")) + "\n";
        let details = json!({
            "cli_version":VERSION,"checkpoint_id":checkpoint,"session_id":entry.state.session_id,
            "strategy":"manual-commit","agent":"Copilot CLI","branch":entry.state.branch,
            "created_at":Utc::now().to_rfc3339(),"files_touched":entry.touched,
            "checkpoints_count":entry.state.checkpoint_count+1,"save_step_count":entry.state.step_count,
            "redaction":{"replacements":redactor.replacements},"compact_transcript_start":0
        });
        files.insert(
            format!("{number}/metadata.json"),
            serde_json::to_string_pretty(&details)? + "\n",
        );
        files.insert(
            format!("{number}/content_hash.txt"),
            format!("sha256:{}\n", hash(&full)),
        );
        files.insert(format!("{number}/full.jsonl"), full);
        files.insert(format!("{number}/transcript.jsonl"), compact);
        files.insert(format!("{number}/prompt.txt"), prompt);
        metadata.push(json!({
            "metadata":format!("/{number}/metadata.json"),"transcript":format!("/{number}/full.jsonl"),
            "compact_transcript":format!("/{number}/transcript.jsonl"),"prompt":format!("/{number}/prompt.txt"),
            "content_hash":format!("/{number}/content_hash.txt")
        }));
    }
    let touched: BTreeSet<_> = entries.iter().flat_map(|e| &e.touched).collect();
    files.insert("metadata.json".into(), serde_json::to_string_pretty(&json!({
        "cli_version":VERSION,"checkpoint_id":checkpoint,"strategy":"manual-commit","branch":entries[0].state.branch,
        "files_touched":touched,"checkpoints_count":1,"sessions":metadata
    }))? + "\n");
    repo.env(&["read-tree", "--empty"], &env)?;
    for (path, content) in files {
        let blob = repo.input(&["hash-object", "-w", "--stdin"], &content)?;
        repo.env(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{path}"),
            ],
            &env,
        )?;
    }
    let tree = repo.env(&["write-tree"], &env)?;
    let mut args = vec!["commit-tree", &tree];
    if old.success {
        args.extend(["-p", old.stdout.trim()]);
    }
    let commit = repo.input(&args, &format!("Trailvault checkpoint {checkpoint}\n"))?;
    let previous = if old.success {
        old.stdout.trim().to_owned()
    } else {
        "0".repeat(commit.len())
    };
    repo.g(&["update-ref", &reference, &commit, &previous])?;
    Ok(reference)
}
pub fn post_commit(repo: &Repo) -> Result<()> {
    crate::policy::enforce_write(repo)?;
    if !crate::policy::capture_allowed(repo)? {
        eprintln!("trailvault: checkpoint creation paused without current policy consent.");
        return Ok(());
    }
    if settings(repo)?["enabled"] == false {
        return Ok(());
    }
    let _lock = repo.lock()?;
    let Some(plan): Option<Plan> = git::optional(&plan_path(repo))? else {
        return Ok(());
    };
    if trailer(
        &repo.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .as_deref()
        != Some(&plan.checkpoint)
    {
        git::remove(&plan_path(repo))?;
        return Ok(());
    }
    ensure!(
        repo.g(&["rev-parse", "HEAD^{tree}"])? == plan.tree,
        "Committed tree differs from prepared checkpoint; pending data retained"
    );
    let entries: Vec<Entry> = plan
        .sessions
        .into_iter()
        .map(|item| {
            let state: State = git::read(&state_path(repo, &item.id)?)?;
            let full = fs::read_to_string(
                state
                    .pending_transcript
                    .as_ref()
                    .context("Missing pending transcript")?,
            )?;
            ensure!(
                hash(&full) == item.transcript_hash,
                "Session changed during commit; pending data retained"
            );
            Ok(Entry {
                state,
                full,
                touched: item.files,
            })
        })
        .collect::<Result<_>>()?;
    ensure!(!entries.is_empty(), "Prepared checkpoint has no sessions");
    let reference = build_checkpoint(repo, &plan.checkpoint, &entries)?;
    let mut queue: Vec<String> = git::optional(&queue_path(repo))?.unwrap_or_default();
    if !queue.contains(&reference) {
        queue.push(reference);
    }
    git::save(&queue_path(repo), &queue)?;
    for entry in entries {
        let mut state = entry.state;
        let mut remaining = vec![];
        for path in &state.files_touched {
            let tree = repo.g(&["ls-tree", "-z", "HEAD", "--", path])?;
            let committed = tree.split_once('\t').and_then(|(head, _)| {
                let mut parts = head.split_whitespace();
                parts.next()?;
                if parts.next()? != "blob" {
                    return None;
                }
                parts.next().map(str::to_owned)
            });
            if committed != state.touched_file_hashes.get(path).cloned().flatten() {
                remaining.push(path.clone());
            }
        }
        state.files_touched = remaining;
        state.checkpoint_count += 1;
        state.last_checkpoint_id = Some(plan.checkpoint.clone());
        state.base_commit = repo.g(&["rev-parse", "HEAD"])?;
        state.step_count = u64::from(!state.files_touched.is_empty());
        if state.files_touched.is_empty() {
            state.prompts.clear();
        }
        git::save(&state_path(repo, &state.session_id)?, &state)?;
    }
    git::remove(&plan_path(repo))
}
pub fn push(repo: &Repo, remote: &str, automatic: bool) -> Result<()> {
    crate::policy::enforce_write(repo)?;
    if !crate::policy::publication_allowed(repo)? {
        ensure!(
            automatic,
            "Checkpoint publication requires current logging consent for this destination. Run `trailvault consent`."
        );
        eprintln!(
            "trailvault: checkpoint publication skipped without current destination consent."
        );
        return Ok(());
    }
    let config = settings(repo)?;
    if config["enabled"] == false
        || (automatic && config.pointer("/strategy_options/push_sessions") == Some(&json!(false)))
        || (automatic && config.pointer("/strategy_options/cloud_artifact") == Some(&json!(true)))
    {
        return Ok(());
    }
    let destination = config["checkpoint_push_remote"].as_str().unwrap_or(remote);
    ensure!(
        !destination.starts_with('-') && !destination.is_empty(),
        "Invalid push remote"
    );
    let pending: Vec<String> = {
        let _lock = repo.lock()?;
        git::optional(&queue_path(repo))?.unwrap_or_default()
    };
    for reference in pending {
        ensure!(
            reference.starts_with("refs/trailvault/checkpoints/"),
            "Invalid queued checkpoint ref"
        );
        let revision = repo.g(&["rev-parse", "--verify", &reference])?;
        let result = git::run(
            &repo.root,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "push",
                "--",
                destination,
                &format!("{reference}:{reference}"),
            ],
            &BTreeMap::new(),
            None,
        )?;
        eprint!("{}", result.stderr);
        if !result.success {
            let message = format!(
                "Checkpoint upload failed for {reference}; durable queue retained. Retry with trailvault push {destination}."
            );
            if !automatic {
                bail!("{message}");
            }
            eprintln!("trailvault: {message}");
            continue;
        }
        let _lock = repo.lock()?;
        if repo.g(&["rev-parse", "--verify", &reference])? == revision {
            let mut queue: Vec<String> = git::optional(&queue_path(repo))?.unwrap_or_default();
            queue.retain(|item| item != &reference);
            git::save(&queue_path(repo), &queue)?;
        }
    }
    Ok(())
}
pub fn fetch(repo: &Repo, remote: &str) -> Result<()> {
    ensure!(
        !remote.starts_with('-') && !remote.is_empty(),
        "Invalid fetch remote"
    );
    let result = git::run(
        &repo.root,
        &[
            "fetch",
            "--",
            remote,
            "+refs/trailvault/checkpoints/*:refs/trailvault/checkpoints/*",
        ],
        &BTreeMap::new(),
        None,
    )?;
    eprint!("{}", result.stderr);
    git::checked(result)?;
    Ok(())
}
pub fn update_local(repo: &Repo, patch: &Value) -> Result<()> {
    let path = repo.root.join(".trailvault/settings.local.json");
    let mut value = git::optional::<Value>(&path)?.unwrap_or(json!({}));
    merge(&mut value, patch)?;
    git::save(&path, &value)
}
pub fn status(repo: &Repo) -> Result<Value> {
    let states: Vec<_> = sessions(repo)?.iter().map(|s| json!({
        "session_id":s.session_id,"phase":s.phase,"files_touched":s.files_touched,"last_checkpoint_id":s.last_checkpoint_id
    })).collect();
    let queue: Vec<String> = git::optional(&queue_path(repo))?.unwrap_or_default();
    Ok(json!({"enabled":settings(repo)?["enabled"],"sessions":states,"queued":queue}))
}
