use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::{capture, git, git::Repo, redact};

pub struct Snapshot {
    pub checkpoint_id: String,
    pub checkpoint_commit: String,
    pub metadata: Value,
    pub content_hash: String,
    pub full: String,
    pub tasks: Vec<Value>,
}

pub struct Options {
    pub working_directory: PathBuf,
    pub allow_lossy: bool,
}

pub struct Prepared {
    pub events: String,
    pub workspace: String,
    pub receipt: Value,
}

fn blob(repo: &Repo, commit: &str, path: &str) -> Result<String> {
    let output = git::run(
        &repo.root,
        &["show", &format!("{commit}:{path}")],
        &Default::default(),
        None,
    )?;
    ensure!(
        output.success,
        "Cannot read checkpoint artifact {path}: {}",
        output.stderr
    );
    // Hashes refer to exact stored bytes, including the final newline.
    Ok(output.stdout)
}

pub fn load(repo: &Repo, checkpoint: &str, session: &str) -> Result<Snapshot> {
    Uuid::parse_str(session).context("Source session identity must be a UUID")?;
    let reference = capture::checkpoint_ref(checkpoint)?;
    let commit = repo
        .g(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
        .context("Checkpoint is not available locally; fetch it first")?;
    let root: Value = serde_json::from_str(&blob(repo, &commit, "metadata.json")?)?;
    ensure!(
        root["checkpoint_id"] == checkpoint,
        "Root checkpoint identity mismatch"
    );
    let sessions = root["sessions"]
        .as_array()
        .context("Missing checkpoint sessions")?;
    let mut selected = None;
    for (number, entry) in sessions.iter().enumerate() {
        let expected = [
            ("metadata", "metadata.json"),
            ("transcript", "full.jsonl"),
            ("content_hash", "content_hash.txt"),
        ];
        for (field, file) in expected {
            ensure!(
                entry[field] == format!("/{number}/{file}"),
                "Invalid checkpoint artifact path: {field}"
            );
        }
        let metadata: Value =
            serde_json::from_str(&blob(repo, &commit, &format!("{number}/metadata.json"))?)?;
        if metadata["session_id"] == session {
            ensure!(
                selected.is_none(),
                "Duplicate source session identity in checkpoint"
            );
            selected = Some(Snapshot {
                checkpoint_id: checkpoint.into(),
                checkpoint_commit: commit.clone(),
                metadata,
                full: blob(repo, &commit, &format!("{number}/full.jsonl"))?,
                content_hash: blob(repo, &commit, &format!("{number}/content_hash.txt"))?
                    .trim()
                    .into(),
                tasks: Vec::new(),
            });
        }
    }
    let mut snapshot = selected.context("Requested session is not in this checkpoint")?;
    let inventory = repo.g(&["ls-tree", "-r", "--name-only", &commit])?;
    for path in inventory
        .lines()
        .filter(|path| path.starts_with("tasks/") && path.ends_with("/task.json"))
    {
        snapshot
            .tasks
            .push(serde_json::from_str(&blob(repo, &commit, path)?)?);
    }
    Ok(snapshot)
}

fn warning(warnings: &mut Vec<Value>, code: &str, message: &str) {
    if !warnings.iter().any(|entry| entry["code"] == code) {
        warnings.push(json!({"code":code,"message":message}));
    }
}

pub fn prepare(snapshot: &Snapshot, options: &Options) -> Result<Prepared> {
    capture::checkpoint_ref(&snapshot.checkpoint_id)?;
    ensure!(
        snapshot.checkpoint_commit.len() == 40
            && snapshot
                .checkpoint_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "Invalid checkpoint commit"
    );
    ensure!(
        snapshot.metadata["checkpoint_id"] == snapshot.checkpoint_id,
        "Session checkpoint identity mismatch"
    );
    ensure!(
        snapshot.metadata["agent"] == "Copilot CLI",
        "Unsupported source agent"
    );
    let source = snapshot.metadata["session_id"]
        .as_str()
        .context("Missing source session identity")?;
    Uuid::parse_str(source).context("Invalid source session identity")?;
    ensure!(
        snapshot.content_hash == format!("sha256:{}", capture::hash(&snapshot.full)),
        "Transcript content hash mismatch"
    );
    ensure!(
        options.working_directory.is_dir(),
        "Working directory must already exist"
    );
    let cwd = options
        .working_directory
        .canonicalize()
        .context("Cannot resolve working directory")?;
    let cwd_text = cwd.to_str().context("Non-UTF8 working directory")?;
    let mut events =
        redact::parse(&snapshot.full).context("Invalid native JSON event object or type")?;
    ensure!(!events.is_empty(), "Transcript is empty");
    ensure!(
        events[0]["type"] == "session.start",
        "Expected native session.start header; compact input is not restorable"
    );
    ensure!(
        events[0]["data"]["version"] == 1,
        "Unsupported native session version"
    );
    ensure!(
        events[0]["data"]["producer"] == "copilot-agent",
        "Unsupported native producer"
    );
    ensure!(
        events[0]["data"]["sessionId"] == source,
        "Native session identity mismatch"
    );
    DateTime::parse_from_rfc3339(
        events[0]["data"]["startTime"]
            .as_str()
            .context("Native header requires startTime")?,
    )
    .context("Invalid native startTime timestamp")?;
    let mut ids = BTreeSet::new();
    let mut started = BTreeSet::new();
    let mut completed = BTreeSet::new();
    let mut warnings = Vec::new();
    if events[0]["data"]["copilotVersion"] != "1.0.105" {
        warning(
            &mut warnings,
            "untested_copilot_version",
            "Native restoration was verified only with Copilot CLI 1.0.105.",
        );
    }
    let replacements = snapshot
        .metadata
        .pointer("/redaction/replacements")
        .and_then(Value::as_u64)
        .context("Missing or invalid redaction replacement count")?;
    if replacements > 0 {
        warning(
            &mut warnings,
            "redacted_content",
            "Redacted source text cannot be recovered.",
        );
    }
    if !snapshot.tasks.is_empty() {
        warning(
            &mut warnings,
            "child_state_unavailable",
            "Child agent runtime state and standalone transcripts are not imported.",
        );
    }
    let new_id = Uuid::new_v4().to_string();
    for (index, event) in events.iter_mut().enumerate() {
        ensure!(
            event.is_object(),
            "Native event must be an object at line {}",
            index + 1
        );
        let id = event["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Missing native event ID")?
            .to_owned();
        Uuid::parse_str(&id).context("Native event ID must be a UUID")?;
        ensure!(!ids.contains(&id), "Native event has a duplicate ID");
        if let Some(parent) = event.get("parentId").filter(|parent| !parent.is_null()) {
            ensure!(
                parent.as_str().is_some_and(|parent| ids.contains(parent)),
                "Missing or forward native parent reference"
            );
        }
        ids.insert(id);
        let timestamp = event["timestamp"]
            .as_str()
            .context("Missing native timestamp")?;
        DateTime::parse_from_rfc3339(timestamp).context("Invalid native timestamp")?;
        let kind = event["type"]
            .as_str()
            .context("Missing native event type")?
            .to_owned();
        ensure!(
            event["data"].is_object(),
            "Native event data must be an object"
        );
        ensure!(
            index == 0 || kind != "session.start",
            "Duplicate session.start header"
        );
        if kind.contains("compaction") || kind.contains("truncation") {
            bail!("Native compaction/truncation restoration is not supported");
        }
        match kind.as_str() {
            "user.message" | "assistant.message" => {
                if kind == "assistant.message" {
                    ensure!(
                        event["data"]["messageId"]
                            .as_str()
                            .is_some_and(|id| !id.is_empty()),
                        "Native assistant message requires messageId"
                    );
                }
                ensure!(
                    event["data"].get("content").is_some_and(Value::is_string),
                    "Native message content must be a string"
                );
                if event["data"].get("attachments").is_some_and(|attachments| {
                    attachments.as_array().is_none_or(|array| !array.is_empty())
                }) {
                    warning(
                        &mut warnings,
                        "attachments_unavailable",
                        "Attachment files are not restored; archived references may be unavailable.",
                    );
                }
            }
            "tool.execution_start" | "tool.execution_complete" => {
                let tool = event["data"]["toolCallId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("Missing tool-call identity")?;
                if kind == "tool.execution_start" {
                    ensure!(
                        started.insert(tool.to_owned()),
                        "Duplicate native tool start"
                    );
                    ensure!(
                        event["data"]["toolName"]
                            .as_str()
                            .is_some_and(|name| !name.is_empty()),
                        "Missing tool name"
                    );
                } else {
                    ensure!(
                        started.contains(tool) && completed.insert(tool.to_owned()),
                        "Orphan or duplicate native tool result"
                    );
                }
            }
            "subagent.started"
            | "subagent.completed"
            | "subagent.configured"
            | "subagent.selected" => {
                warning(
                    &mut warnings,
                    "child_state_unavailable",
                    "Child agent runtime state and standalone transcripts are not imported.",
                );
            }
            "session.start"
            | "session.resume"
            | "session.model_change"
            | "session.usage_record"
            | "session.shutdown"
            | "session.error"
            | "session.info"
            | "session.idle"
            | "system.message"
            | "assistant.turn_start"
            | "assistant.turn_end"
            | "hook.start"
            | "hook.end" => {}
            _ => warning(
                &mut warnings,
                "unknown_event",
                "Unknown native events are preserved but their resumability is unverified.",
            ),
        }
        if matches!(kind.as_str(), "session.start" | "session.resume") {
            let data = event["data"]
                .as_object_mut()
                .context("Missing session data")?;
            if let Some(identity) = data.get("sessionId") {
                ensure!(
                    identity == source,
                    "Resumed native session identity mismatch"
                );
                data.insert("sessionId".into(), json!(new_id));
            }
            data.insert("context".into(), json!({"cwd":cwd_text}));
            if kind == "session.start" {
                data.insert("alreadyInUse".into(), json!(false));
                data.insert("remoteSteerable".into(), json!(false));
            }
        }
    }
    if started != completed {
        warning(
            &mut warnings,
            "incomplete_tools",
            "Snapshot has incomplete tool calls; no tool is restarted or result fabricated by Trailvault.",
        );
    }
    ensure!(
        options.allow_lossy || warnings.is_empty(),
        "Restoration requires --allow-lossy: {}",
        serde_json::to_string(&warnings)?
    );
    warning(
        &mut warnings,
        "runtime_state_not_restored",
        "Experimental native-file import: caches, hidden state, permissions, hooks and rewind snapshots are not restored.",
    );
    warning(
        &mut warnings,
        "workspace_not_rewound",
        "The target working tree is not modified or checked out to the captured revision.",
    );
    warning(
        &mut warnings,
        "fork_not_sync",
        "A new local session is created; this does not synchronize or overwrite the source conversation.",
    );
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let created = events[0]["data"]["startTime"]
        .as_str()
        .context("Missing created timestamp")?;
    let created = DateTime::parse_from_rfc3339(created)?
        .to_utc()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // JSON string literals are valid YAML scalars, including quotes/newlines in paths.
    let workspace = format!(
        "id: {}\ncwd: {}\ncreated_at: {}\nupdated_at: {}\n",
        serde_json::to_string(&new_id)?,
        serde_json::to_string(cwd_text)?,
        serde_json::to_string(&created)?,
        serde_json::to_string(&now)?
    );
    Ok(Prepared {
        events: redact::jsonl(&events)?,
        workspace,
        receipt: json!({"version":1,"kind":"experimental-native-fork","restored_session_id":new_id,
            "source_session_id":source,"checkpoint_id":snapshot.checkpoint_id,
            "checkpoint_commit":snapshot.checkpoint_commit,"source_content_hash":snapshot.content_hash,
            "working_directory":cwd,"created_at":now,"warnings":warnings}),
    })
}

fn directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Destination must be a directory, not a symlink: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().context("Destination has no parent")?;
            directory(parent)?;
            DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub fn write(prepared: &Prepared, home: &Path) -> Result<PathBuf> {
    ensure!(
        home.is_absolute(),
        "Copilot home must be an explicit absolute path"
    );
    let id = prepared.receipt["restored_session_id"]
        .as_str()
        .context("Missing restored identity")?;
    Uuid::parse_str(id).context("Invalid restored identity")?;
    directory(home)?;
    let sessions = home.join("session-state");
    directory(&sessions)?;
    let target = sessions.join(id);
    ensure!(
        !target.try_exists()? && fs::symlink_metadata(&target).is_err(),
        "Existing session must not be overwritten"
    );
    let stage = tempfile::Builder::new()
        .prefix(".trailvault-restore-")
        .tempdir_in(&sessions)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    for (name, contents) in [
        ("events.jsonl", prepared.events.clone()),
        ("workspace.yaml", prepared.workspace.clone()),
        (
            "trailvault-restore.json",
            serde_json::to_string_pretty(&prepared.receipt)? + "\n",
        ),
    ] {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(stage.path().join(name))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    // Serialize cooperating restorers; the UUID is generated before this publication.
    let lock_path = sessions.join(".trailvault-restore.lock");
    ensure!(
        !fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "Restoration lock must not be a symlink"
    );
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    ensure!(
        fs::symlink_metadata(&target).is_err(),
        "Existing session must not be overwritten"
    );
    fs::rename(stage.path(), &target)?;
    fs::File::open(&sessions)?.sync_all()?;
    Ok(target)
}
