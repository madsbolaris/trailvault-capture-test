use crate::{
    capture,
    git::{self, Repo},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session-start"),
    ("userPromptSubmitted", "user-prompt-submitted"),
    ("agentStop", "agent-stop"),
    ("sessionEnd", "session-end"),
    ("subagentStop", "subagent-stop"),
    ("preToolUse", "pre-tool-use"),
    ("postToolUse", "post-tool-use"),
    ("errorOccurred", "error-occurred"),
];

pub fn home() -> Result<PathBuf> {
    let path = match env::var_os("COPILOT_HOME") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env::var_os("HOME").context("HOME is unset")?).join(".copilot"),
    };
    ensure!(path.is_absolute(), "Copilot home must be an absolute path");
    Ok(path)
}

pub fn path() -> Result<PathBuf> {
    let path = home()?.join("hooks/trailvault.json");
    ensure_plain(path.parent().context("Missing hooks directory")?, true)?;
    Ok(path)
}

pub fn ensure_plain(path: &Path, directory: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(
                !metadata.file_type().is_symlink()
                    && if directory {
                        metadata.is_dir()
                    } else {
                        metadata.is_file()
                    },
                "Refusing unexpected or symlinked installation path {}",
                path.display()
            );
            Ok(())
        }
    }
}

pub fn validate_repository_paths(repo: &Repo) -> Result<()> {
    for path in [".trailvault", ".github", ".github/hooks"] {
        ensure_plain(&repo.root.join(path), true)?;
    }
    for path in [
        ".trailvault/settings.json",
        ".trailvault/settings.local.json",
    ] {
        ensure_plain(&repo.root.join(path), false)?;
    }
    Ok(())
}

pub fn configuration(executable: &Path, global: bool) -> Result<Value> {
    let executable = executable
        .to_str()
        .context("Executable path is not UTF-8")?;
    let quoted = format!("'{}'", executable.replace('\'', "'\\''"));
    let hooks:serde_json::Map<String,Value>=EVENTS.iter().map(|(event,command)| {
        ((*event).into(),json!([{"type":"command","bash":format!("{quoted} hooks copilot-cli {command}{}",if global {" --global"} else {""}),"comment":"Trailvault CLI"}]))
    }).collect();
    let mut value = json!({"version":1,"hooks":hooks});
    if global {
        value["trailvault_managed"] = json!(1);
    }
    Ok(value)
}

fn owned(value: &Value, global: bool) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if value["version"] != 1
        || object
            .keys()
            .any(|key| !["version", "hooks", "trailvault_managed"].contains(&key.as_str()))
    {
        return false;
    }
    if global && value["trailvault_managed"] != 1 {
        return false;
    }
    let Some(hooks) = value["hooks"].as_object() else {
        return false;
    };
    if !(7..=8).contains(&hooks.len()) {
        return false;
    }
    for (event, command) in EVENTS {
        let Some(entries) = hooks.get(*event) else {
            if *event == "subagentStop" {
                continue;
            } else {
                return false;
            }
        };
        let Some(entries) = entries.as_array() else {
            return false;
        };
        if entries.len() != 1 {
            return false;
        }
        let entry = &entries[0];
        let Some(object) = entry.as_object() else {
            return false;
        };
        if object.len() != 3 || entry["type"] != "command" || entry["comment"] != "Trailvault CLI" {
            return false;
        }
        let Some(bash) = entry["bash"].as_str() else {
            return false;
        };
        let suffix = format!(
            "' hooks copilot-cli {command}{}",
            if global { " --global" } else { "" }
        );
        if !bash.starts_with('\'') || !bash.ends_with(&suffix) {
            return false;
        }
    }
    hooks
        .keys()
        .all(|key| EVENTS.iter().any(|(event, _)| event == key))
}

fn read_owned(path: &Path, global: bool) -> Result<Option<Value>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Refusing nonregular hook file {}",
                path.display()
            );
            let value: Value = git::read(path)?;
            ensure!(
                owned(&value, global),
                "Hook file is not an unmodified Trailvault installation: {}",
                path.display()
            );
            Ok(Some(value))
        }
    }
}

pub fn installed() -> Result<bool> {
    Ok(read_owned(&path()?, true)?.is_some())
}

pub fn install(executable: &Path) -> Result<()> {
    let path = path()?;
    let directory = path.parent().context("Hook has no directory")?;
    git::mkdir_private(directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(directory.join(".trailvault-install.lock"))?;
    lock.lock_exclusive()?;
    read_owned(&path, true)?;
    git::save(&path, &configuration(&executable.canonicalize()?, true)?)?;
    println!(
        "Installed Copilot CLI hooks for this user: {}\nSessions are captured locally by default. Repository Git hooks are installed on the next captured session.\nTo publish transcripts, explicitly run `trailvault enable --push-sessions` in that repository.\nTo opt out of capture, run `trailvault disable` there. Restart Copilot to load changed hooks.",
        path.display()
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = path()?;
    if !path
        .parent()
        .context("Missing hook directory")?
        .try_exists()?
    {
        println!("Copilot CLI user hooks are not installed.");
        return Ok(());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path.with_file_name(".trailvault-install.lock"))?;
    lock.lock_exclusive()?;
    if read_owned(&path, true)?.is_some() {
        git::remove(&path)?;
    }
    println!(
        "Removed Copilot CLI user hooks. Existing local captures and repository Git hooks are retained; queued checkpoints may still publish if previously enabled. Restart Copilot."
    );
    Ok(())
}

pub fn remove_repository_registration(repo: &Repo) -> Result<()> {
    let path = repo.root.join(".github/hooks/trailvault.json");
    if read_owned(&path, false)?.is_some() {
        git::remove(&path)?;
        eprintln!(
            "trailvault: removed duplicate repository Copilot registration; user hooks handle this repository."
        );
    }
    Ok(())
}

pub fn dispatch(name: &str, input: &Value, executable: &Path) -> Result<()> {
    ensure!(
        EVENTS.iter().any(|(_, command)| *command == name),
        "Unsupported Copilot hook {name}"
    );
    ensure!(input.is_object(), "Hook payload must be an object");
    if let Some(id) = input.get("sessionId") {
        let id = id.as_str().context("sessionId must be a string")?;
        ensure!(
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)),
            "Invalid session ID"
        );
    }
    let cwd = input
        .get("cwd")
        .map(|value| {
            value
                .as_str()
                .context("cwd must be a string")
                .map(PathBuf::from)
        })
        .transpose()?
        .unwrap_or(env::current_dir()?);
    ensure!(cwd.is_absolute(), "cwd must be absolute");
    let probe = git::run(
        &cwd,
        &["rev-parse", "--is-inside-work-tree"],
        &Default::default(),
        None,
    )?;
    if !probe.success {
        ensure!(
            probe.stderr.contains("not a git repository")
                || probe.stderr.contains("safe.bareRepository"),
            "Cannot inspect hook repository: {}",
            probe.stderr
        );
        eprintln!("trailvault: skipped capture outside a Git working tree.");
        return Ok(());
    }
    if probe.stdout.trim() != "true" {
        eprintln!("trailvault: skipped capture in a bare Git repository.");
        return Ok(());
    }
    let repo = Repo::discover(&cwd)?;
    let head = git::run(
        &repo.root,
        &["rev-parse", "--verify", "HEAD"],
        &Default::default(),
        None,
    )?;
    if !head.success {
        eprintln!("trailvault: capture requires an initial Git commit; skipped this session.");
        return Ok(());
    }
    if !installed()? {
        eprintln!("trailvault: user hook installation is absent; skipped stale global hook.");
        return Ok(());
    }
    if name == "session-start" || name == "user-prompt-submitted" {
        ensure!(
            input["sessionId"].as_str().is_some_and(|id| !id.is_empty()),
            "sessionId is required"
        );
        if name == "user-prompt-submitted" {
            ensure!(input["prompt"].is_string(), "prompt must be a string");
        }
    }
    validate_repository_paths(&repo)?;
    crate::policy::observe(&repo, executable)?;
    if !crate::policy::capture_allowed(&repo)? {
        eprintln!("trailvault: capture skipped until repository logging consent is granted.");
        return Ok(());
    }
    let config = capture::settings_with_defaults(
        &repo,
        json!({"enabled":true,"commit_linking":"prompt","strategy_options":{"push_sessions":false}}),
    )?;
    if config["enabled"] == false {
        eprintln!("trailvault: repository opted out of capture.");
        return Ok(());
    }
    if name == "session-start" || name == "user-prompt-submitted" {
        capture::enable_local(&repo, executable)?;
    }
    if capture::settings(&repo)?["enabled"] == false {
        eprintln!("trailvault: repository is not initialized; waiting for session-start.");
        return Ok(());
    }
    capture::agent_hook(&repo, name, input)
}
