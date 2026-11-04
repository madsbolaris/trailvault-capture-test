use super::{Blob, Checkpoint, Run, corpus::Case, digest, project};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use trailvault::git;

#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub binary: PathBuf,
}

impl Tool {
    pub fn new(name: &str) -> Result<Self> {
        ensure!(
            matches!(name, "entire" | "trailvault"),
            "Unknown target {name}"
        );
        let variable = format!("{}_BIN", name.to_uppercase());
        let binary = env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| {
            if name == "entire" {
                PathBuf::from("entire")
            } else {
                project().join("target/debug/trailvault")
            }
        });
        let binary = if binary.is_file() {
            binary.canonicalize()?
        } else {
            env::split_paths(&env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(&binary))
                .find(|path| path.is_file())
                .with_context(|| format!("Cannot find {}", binary.display()))?
                .canonicalize()?
        };
        Ok(Self {
            name: name.into(),
            binary,
        })
    }

    pub fn sha256(&self) -> Result<String> {
        Ok(digest(&fs::read(&self.binary)?))
    }
}

struct Workspace {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    env: BTreeMap<String, String>,
    tool: Tool,
}

impl Workspace {
    fn new(tool: &Tool, case: &Case) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repo = root.join("work");
        let home = root.join("copilot");
        fs::create_dir(&repo)?;
        fs::create_dir(&home)?;
        let path = env::join_paths(
            std::iter::once(
                tool.binary
                    .parent()
                    .context("Binary has no parent")?
                    .to_owned(),
            )
            .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
        )?;
        let isolated_home = root.join("home");
        fs::create_dir(&isolated_home)?;
        let env = BTreeMap::from([
            (
                "PATH".into(),
                path.to_str().context("Non-UTF8 PATH")?.into(),
            ),
            ("HOME".into(), isolated_home.display().to_string()),
            (
                "XDG_CONFIG_HOME".into(),
                root.join("config").display().to_string(),
            ),
            ("COPILOT_HOME".into(), home.display().to_string()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_AUTHOR_DATE".into(), "2026-01-02T03:04:05Z".into()),
            ("GIT_COMMITTER_DATE".into(), "2026-01-02T03:04:05Z".into()),
            ("ENTIRE_TELEMETRY_ENABLED".into(), "false".into()),
            ("CI".into(), "true".into()),
        ]);
        let workspace = Self {
            _temp: temp,
            repo,
            home,
            env,
            tool: tool.clone(),
        };
        workspace.g(&["init", "-b", "main"])?;
        workspace.g(&["config", "user.name", "Schema Fixture"])?;
        workspace.g(&["config", "user.email", "schema@example.invalid"])?;
        workspace.g(&["config", "commit.gpgsign", "false"])?;
        fs::write(workspace.repo.join("tracked.txt"), "baseline\n")?;
        workspace.g(&["add", "."])?;
        workspace.g(&["commit", "-m", "Initial fixture"])?;
        workspace.cli(
            &[
                "enable",
                "--agent",
                "copilot-cli",
                "--absolute-git-hook-path",
                "--telemetry=false",
            ],
            None,
        )?;
        git::save(
            &workspace
                .repo
                .join(format!(".{}/settings.local.json", tool.name)),
            &case.settings,
        )?;
        workspace.g(&["add", "."])?;
        workspace.g(&["commit", "-m", "Configure fixture"])?;
        Ok(workspace)
    }

    fn command(&self, binary: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        git::checked(git::command(
            &self.repo,
            binary,
            args,
            &self.env,
            input,
            Duration::from_secs(90),
        )?)
        .with_context(|| format!("{} {}", binary.display(), args.join(" ")))
    }

    fn g(&self, args: &[&str]) -> Result<String> {
        self.command(Path::new("git"), args, None)
    }

    fn cli(&self, args: &[&str], payload: Option<&Value>) -> Result<String> {
        self.command(
            &self.tool.binary,
            args,
            payload.map(serde_json::to_vec).transpose()?.as_deref(),
        )
    }

    fn hook(&self, name: &str, payload: &Value) -> Result<()> {
        self.cli(&["hooks", "copilot-cli", name], Some(payload))?;
        Ok(())
    }

    fn checkpoint(&self, message: &str) -> Result<Checkpoint> {
        self.g(&["commit", "-m", message])?;
        let source_message = self.g(&["log", "-1", "--format=%B"])?;
        let trailer = if self.tool.name == "entire" {
            "Entire-Checkpoint"
        } else {
            "Trailvault-Checkpoint"
        };
        let id = trailvault::capture::trailer(&source_message, trailer)
            .context("Source commit has no checkpoint trailer")?;
        let reference = format!(
            "refs/{}/checkpoints/{}/{id}",
            self.tool.name,
            &id[id.len().checked_sub(2).context("Checkpoint ID too short")?..]
        );
        let tree = self.g(&["ls-tree", "-r", "--full-tree", &reference])?;
        let mut files = BTreeMap::new();
        for line in tree.lines() {
            let (entry, path) = line.split_once('\t').context("Invalid ls-tree entry")?;
            let fields: Vec<_> = entry.split_whitespace().collect();
            ensure!(
                fields.len() == 3 && fields[1] == "blob",
                "Unexpected tree entry {line}"
            );
            // Read binary blobs directly: git::checked would trim final newlines.
            let output = Command::new("git")
                .args(["cat-file", "blob", fields[2]])
                .current_dir(&self.repo)
                .envs(&self.env)
                .output()?;
            ensure!(
                output.status.success(),
                "Cannot read blob {path}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            files.insert(path.into(), Blob::new(fields[0], output.stdout));
        }
        ensure!(!files.is_empty(), "Empty checkpoint tree");
        Ok(Checkpoint { id, files })
    }
}

pub fn collect(tool: &Tool, case: &Case) -> Result<Run> {
    let workspace = Workspace::new(tool, case).context("Creating isolated repository")?;
    let mut native = vec![];
    let mut checkpoints = vec![];
    let turns = case
        .sessions
        .iter()
        .map(|session| session.turns.len())
        .max()
        .context("No sessions")?;
    let mut transcripts = Vec::new();
    for (index, _) in case.sessions.iter().enumerate() {
        let id = format!("20000000-0000-4000-8000-{index:012}");
        let path = workspace
            .home
            .join("session-state")
            .join(&id)
            .join("events.jsonl");
        fs::create_dir_all(path.parent().context("Transcript has no parent")?)?;
        let header = json!({"id":"event-start","parentId":null,"type":"session.start","timestamp":"2026-01-02T03:04:05Z","data":{"sessionId":id,"copilotVersion":"1.0.105","context":{"cwd":"/fixture/worktree","branch":"main"}}});
        let mut bytes = serde_json::to_vec(&header)?;
        bytes.push(b'\n');
        fs::write(&path, &bytes)?;
        let payload = json!({"sessionId":id,"cwd":workspace.repo,"transcriptPath":path,"timestamp":1767323045000_u64});
        workspace.hook("session-start", &payload)?;
        transcripts.push((path, payload, bytes));
    }
    for turn_index in 0..turns {
        for (session_index, session) in case.sessions.iter().enumerate() {
            let Some(turn) = session.turns.get(turn_index) else {
                continue;
            };
            let (path, payload, bytes) = &mut transcripts[session_index];
            for (event_index, raw) in turn.events.iter().enumerate() {
                let mut event = raw.clone();
                event["id"] = json!(format!("event-{session_index}-{turn_index}-{event_index}"));
                event["parentId"] = if event_index == 0 {
                    Value::Null
                } else {
                    json!(format!(
                        "event-{session_index}-{turn_index}-{}",
                        event_index - 1
                    ))
                };
                if event.get("timestamp").is_none() {
                    event["timestamp"] = json!(format!("2026-01-02T03:04:{:02}Z", 6 + event_index));
                } else if event["timestamp"].is_null() {
                    event
                        .as_object_mut()
                        .context("Event must be an object")?
                        .remove("timestamp");
                }
                // Deliberately noncanonical native formatting and final whitespace.
                let line = if case.name == "native-format-whitespace-escapes" {
                    format!("  {}  \r\n", serde_json::to_string(&event)?)
                } else {
                    format!("{}\n", serde_json::to_string(&event)?)
                };
                bytes.extend_from_slice(line.as_bytes());
            }
            fs::write(&*path, &*bytes)?;
            payload["prompt"] = json!(turn.prompt);
            workspace.hook("user-prompt-submitted", payload)?;
            fs::write(
                workspace.repo.join(format!("agent-{session_index}.txt")),
                format!("Turn {turn_index}\n"),
            )?;
            if case.partial_commit {
                fs::write(workspace.repo.join("partial.txt"), "Pending second file\n")?;
            }
            for id in &session.subagent_stops {
                let mut stop = payload.clone();
                stop["agentId"] = json!(id);
                stop["agentName"] = json!("explore");
                workspace
                    .hook("subagent-stop", &stop)
                    .context("Subagent lifecycle hook")?;
            }
            workspace.hook("agent-stop", payload)?;
            workspace.g(&["add", &format!("agent-{session_index}.txt")])?;
        }
        checkpoints.push(workspace.checkpoint(&format!("Fixture turn {turn_index}"))?);
    }
    if case.partial_commit {
        workspace.g(&["add", "partial.txt"])?;
        checkpoints.push(workspace.checkpoint("Remaining agent file")?);
    }
    for (path, payload, expected) in &transcripts {
        workspace.hook("session-end", payload)?;
        ensure!(
            fs::read(path)? == *expected,
            "Capture modified the native transcript"
        );
        native.push(digest(expected));
    }
    Ok(Run {
        checkpoints,
        native,
    })
}
