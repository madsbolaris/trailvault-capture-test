#![allow(dead_code)]
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::TempDir;
use trailvault::{capture, git};
use uuid::Uuid;

pub fn project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
pub fn target() -> String {
    env::var("TRAILVAULT_TARGET").unwrap_or("trailvault".into())
}
pub fn executable() -> Result<PathBuf> {
    let target = target();
    ensure!(
        ["entire", "trailvault"].contains(&target.as_str()),
        "Unknown target {target}"
    );
    if target == "entire" {
        return Ok(env::var_os("ENTIRE_BIN")
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from("entire")));
    }
    Ok(env::var_os("TRAILVAULT_BIN")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_trailvault").map(PathBuf::from))
        .unwrap_or_else(|| project().join("target/debug/trailvault")))
}
pub fn trailer_name() -> &'static str {
    if target() == "entire" {
        "Entire-Checkpoint"
    } else {
        "Trailvault-Checkpoint"
    }
}
pub fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
pub fn command(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    input: Option<&[u8]>,
) -> Result<String> {
    git::checked(git::command(
        cwd,
        binary,
        args,
        env,
        input,
        Duration::from_secs(180),
    )?)
}
pub fn g(cwd: &Path, args: &[&str]) -> Result<String> {
    command(cwd, Path::new("git"), args, &BTreeMap::new(), None)
}
pub fn invoke(
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    input: Option<&Value>,
) -> Result<String> {
    let bytes = input.map(serde_json::to_vec).transpose()?;
    command(cwd, &executable()?, args, env, bytes.as_deref())
}
pub fn put(path: &Path, content: impl AsRef<[u8]>) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing parent")?)?;
    fs::write(path, content)?;
    Ok(())
}
pub fn script(path: &Path, content: &str) -> Result<()> {
    put(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}
pub fn event(kind: &str, data: Value) -> Value {
    json!({"type":kind,"data":data,"id":Uuid::new_v4().to_string(),"timestamp":Utc::now().to_rfc3339(),"parentId":null})
}
pub fn report(name: &str, value: &Value) -> Result<()> {
    git::save(&project().join(".test-results").join(name), value)
}

pub struct Fixture {
    pub temp: TempDir,
    pub repo: PathBuf,
    pub env: BTreeMap<String, String>,
}
impl Fixture {
    pub fn bare() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repo = root.join("work");
        fs::create_dir(&repo)?;
        let home = root.join("agent");
        let binary = executable()?;
        let path = env::join_paths(
            binary
                .parent()
                .into_iter()
                .map(Path::to_owned)
                .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
        )?;
        let env = BTreeMap::from([
            ("COPILOT_HOME".into(), text(&home)),
            (
                "PATH".into(),
                path.to_str().context("PATH is not UTF-8")?.into(),
            ),
            ("ENTIRE_TELEMETRY_ENABLED".into(), "false".into()),
        ]);
        let fixture = Self { temp, repo, env };
        fixture.g(&["init", "-b", "main"])?;
        fixture.g(&["config", "user.name", "Trailvault Test"])?;
        fixture.g(&["config", "user.email", "trailvault@example.invalid"])?;
        fixture.g(&["config", "commit.gpgsign", "false"])?;
        fixture.g(&["config", "core.quotePath", "false"])?;
        put(&fixture.repo.join("tracked.txt"), "original\n")?;
        put(&fixture.repo.join("old-name.txt"), "rename content\n")?;
        fixture.g(&["add", "."])?;
        fixture.g(&["commit", "-m", "Initial fixture"])?;
        Ok(fixture)
    }
    pub fn new() -> Result<Self> {
        let f = Self::bare()?;
        f.enable()?;
        Ok(f)
    }
    pub fn enable(&self) -> Result<()> {
        self.cli(&[
            "enable",
            "--agent",
            "copilot-cli",
            "--absolute-git-hook-path",
            "--telemetry=false",
        ])?;
        self.configure(&json!({"enabled":true,"commit_linking":"always"}))?;
        self.g(&["add", "."])?;
        self.g(&["commit", "-m", "Configure capture"])?;
        Ok(())
    }
    pub fn g(&self, args: &[&str]) -> Result<String> {
        command(&self.repo, Path::new("git"), args, &self.env, None)
    }
    pub fn cli(&self, args: &[&str]) -> Result<String> {
        invoke(&self.repo, args, &self.env, None)
    }
    pub fn configure(&self, value: &Value) -> Result<()> {
        git::save(
            &self.repo.join(format!(".{}/settings.local.json", target())),
            value,
        )
    }
    pub fn hook(&self, name: &str, payload: &Value) -> Result<String> {
        invoke(
            &self.repo,
            &["hooks", "copilot-cli", name],
            &self.env,
            Some(payload),
        )
    }
    pub fn remote(&self) -> Result<PathBuf> {
        let path = self.temp.path().join("remote.git");
        g(self.temp.path(), &["init", "--bare", &text(&path)])?;
        self.g(&["remote", "add", "origin", &text(&path)])?;
        Ok(path)
    }
    pub fn capture(&self, file: &str) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        let transcript = PathBuf::from(&self.env["COPILOT_HOME"])
            .join("session-state")
            .join(&id)
            .join("events.jsonl");
        let prompt = "Synthetic capture test; the weather is pleasant";
        let events = [
            event(
                "session.start",
                json!({"sessionId":id,"context":{"cwd":self.repo}}),
            ),
            event("user.message", json!({"content":prompt})),
            event("assistant.message", json!({"content":"Done"})),
        ];
        put(&transcript, trailvault::redact::jsonl(&events)?)?;
        let mut payload = json!({"sessionId":id,"cwd":self.repo,"transcriptPath":transcript});
        self.hook("session-start", &payload)?;
        payload["prompt"] = json!(prompt);
        self.hook("user-prompt-submitted", &payload)?;
        put(&self.repo.join(file), "agent change\n")?;
        self.hook("agent-stop", &payload)?;
        Ok(Session {
            id,
            transcript,
            payload,
        })
    }
    pub fn commit_capture(&self) -> Result<(Session, String)> {
        let session = self.capture("agent.txt")?;
        self.g(&["add", "agent.txt"])?;
        self.g(&["commit", "-m", "Agent changes"])?;
        let checkpoint = capture::trailer(&self.g(&["log", "-1", "--format=%B"])?, trailer_name())
            .context("Missing checkpoint")?;
        Ok((
            session,
            format!(
                "refs/{}/checkpoints/{}/{checkpoint}",
                target(),
                &checkpoint[24..]
            ),
        ))
    }
    pub fn status(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.cli(&["status"])?)?)
    }
}
pub struct Session {
    pub id: String,
    pub transcript: PathBuf,
    pub payload: Value,
}
