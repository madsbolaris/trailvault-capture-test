mod support;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use support::{Fixture, put};
use trailvault::git;

struct Test {
    fixture: Fixture,
    home: PathBuf,
    real: PathBuf,
    env: BTreeMap<String, String>,
}

impl Test {
    fn new(mode: Option<&str>) -> Result<Self> {
        let mut fixture = Fixture::bare()?;
        let home = fixture.temp.path().join("user's home");
        fs::create_dir(&home)?;
        let real = home.join("real-bin/gh");
        support::script(
            &real,
            r#"#!/bin/sh
printf '%s\0' "$@" >> "$GH_FIXTURE_CALLS"
if [ "$1" = repo ] && [ "$2" = clone ] && [ "$GH_FIXTURE_FAIL" != yes ]; then
  for arg in "$@"; do
    if [ "$arg" = --help ]; then printf 'real gh clone help\n'; exit 0; fi
    if [ "$arg" = --bare ]; then exec git clone --bare -- "$GH_FIXTURE_REMOTE" "$GH_FIXTURE_DEST"; fi
  done
  exec git -c core.hooksPath=/dev/null clone -- "$GH_FIXTURE_REMOTE" "$GH_FIXTURE_DEST"
fi
printf 'real gh stdout\n'
printf 'real gh stderr\n' >&2
exit "${GH_FIXTURE_EXIT:-0}"
"#,
        )?;
        let mut env = fixture.env.clone();
        env.insert("HOME".into(), support::text(&home));
        env.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env.insert(
            "GH_FIXTURE_CALLS".into(),
            support::text(&home.join("gh-calls")),
        );
        env.insert("GH_FIXTURE_REMOTE".into(), support::text(&fixture.repo));
        env.insert("GH_FIXTURE_DEST".into(), "checkout".into());
        let path = std::env::join_paths(
            [
                home.join(".local/bin"),
                real.parent().context("No real parent")?.to_owned(),
            ]
            .into_iter()
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )),
        )?;
        env.insert(
            "PATH".into(),
            path.to_str().context("Non-UTF8 PATH")?.into(),
        );
        fixture.env = env.clone();
        if let Some(mode) = mode {
            git::save(
                &fixture.repo.join(".trailvault/policy.json"),
                &json!({"version":1,"logging":mode,"destination":"origin","description":"Synthetic team logging policy."}),
            )?;
            fixture.g(&["add", ".trailvault/policy.json"])?;
            fixture.g(&["commit", "-m", "Declare logging policy"])?;
        }
        Ok(Self {
            fixture,
            home,
            real,
            env,
        })
    }

    fn cli(&self, cwd: &Path, args: &[&str]) -> Result<git::Output> {
        git::command(
            cwd,
            &support::executable()?,
            args,
            &self.env,
            None,
            Duration::from_secs(30),
        )
    }

    fn install(&self) -> Result<()> {
        git::checked(self.cli(
            &self.home,
            &[
                "install",
                "--gh-wrapper",
                "--gh-bin",
                support::text(&self.real).as_str(),
            ],
        )?)?;
        Ok(())
    }

    fn shim(&self) -> PathBuf {
        self.home.join(".local/bin/gh")
    }
    fn checkout(&self) -> PathBuf {
        self.home.join("checkout")
    }

    fn gh(&self, args: &[&str]) -> Result<git::Output> {
        git::command(
            &self.home,
            &self.shim(),
            args,
            &self.env,
            None,
            Duration::from_secs(30),
        )
    }

    fn clone(&self) -> Result<git::Output> {
        self.gh(&["repo", "clone", "fixture/project", "checkout"])
    }

    fn decision(&self, flag: &str) -> Result<()> {
        git::checked(self.cli(&self.checkout(), &["consent", flag])?)?;
        Ok(())
    }

    fn g(&self, args: &[&str]) -> Result<git::Output> {
        git::command(
            &self.checkout(),
            Path::new("git"),
            args,
            &self.env,
            None,
            Duration::from_secs(30),
        )
    }

    fn stage(&self) -> Result<()> {
        git::checked(self.g(&["config", "user.name", "Wrapper Test"])?)?;
        git::checked(self.g(&["config", "user.email", "wrapper@example.invalid"])?)?;
        git::checked(self.g(&["config", "commit.gpgsign", "false"])?)?;
        put(&self.checkout().join("change.txt"), "manual change\n")?;
        git::checked(self.g(&["add", "change.txt"])?)?;
        Ok(())
    }

    fn recorded(&self) -> Result<Vec<String>> {
        Ok(fs::read(self.home.join("gh-calls"))?
            .split(|byte| *byte == 0)
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| String::from_utf8(bytes.to_vec()))
            .collect::<std::result::Result<_, _>>()?)
    }
}

#[test]
fn installs_normal_gh_command_without_overwriting_real_binary_and_is_repeatable() -> Result<()> {
    let f = Test::new(None)?;
    let original = fs::read(&f.real)?;
    f.install()?;
    let first = fs::read(f.shim())?;
    f.install()?;
    ensure!(first == fs::read(f.shim())? && original == fs::read(&f.real)?);
    ensure!(f.gh(&["--version"])?.success);
    Ok(())
}

#[test]
fn delegates_nonclone_arguments_stdout_stderr_and_exit_code_unchanged() -> Result<()> {
    let mut f = Test::new(None)?;
    f.install()?;
    f.env.insert("GH_FIXTURE_EXIT".into(), "7".into());
    let args = [
        "issue",
        "create",
        "--title",
        "spaces ' and ; $ symbols",
        "--body",
        "line one\nline two",
    ];
    let output = f.gh(&args)?;
    ensure!(
        !output.success
            && output.stdout == "real gh stdout\n"
            && output.stderr == "real gh stderr\n"
    );
    ensure!(f.recorded()? == args);
    let status = std::process::Command::new(f.shim())
        .args(["--version"])
        .envs(&f.env)
        .current_dir(&f.home)
        .status()?;
    ensure!(status.code() == Some(7));
    Ok(())
}

#[test]
fn clone_help_and_failed_clones_do_not_create_consent_or_mutate_checkout() -> Result<()> {
    let mut f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(f.gh(&["repo", "clone", "--help"])?.stdout == "real gh clone help\n");
    f.env.insert("GH_FIXTURE_FAIL".into(), "yes".into());
    f.env.insert("GH_FIXTURE_EXIT".into(), "23".into());
    let status = std::process::Command::new(f.shim())
        .args(["repo", "clone", "fixture/project", "checkout"])
        .envs(&f.env)
        .current_dir(&f.home)
        .status()?;
    ensure!(status.code() == Some(23) && !f.checkout().exists());
    Ok(())
}

#[test]
fn clones_without_policy_keep_normal_gh_behavior_and_forward_clone_flags() -> Result<()> {
    let f = Test::new(None)?;
    f.install()?;
    let args = [
        "repo",
        "clone",
        "fixture/project",
        "checkout",
        "--no-upstream",
        "--",
        "--depth=1",
    ];
    let output = f.gh(&args)?;
    ensure!(output.success, "{}", output.stderr);
    ensure!(f.recorded()? == args);
    ensure!(f.checkout().join("tracked.txt").exists());
    ensure!(!f.checkout().join(".git/trailvault-consent.json").exists());
    Ok(())
}

#[test]
fn required_policy_without_a_terminal_stays_pending_and_blocks_commits_and_pushes() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    let output = f.clone()?;
    ensure!(
        !output.success && output.stderr.contains("consent"),
        "{}",
        output.stderr
    );
    ensure!(f.checkout().exists());
    f.stage()?;
    let commit = f.g(&["commit", "-m", "Must fail"])?;
    ensure!(
        !commit.success && commit.stderr.contains("requires"),
        "{}",
        commit.stderr
    );
    let push = f.g(&["push", "origin", "HEAD:refs/heads/pending-test"])?;
    ensure!(
        !push.success && push.stderr.contains("requires"),
        "{}",
        push.stderr
    );
    Ok(())
}

#[test]
fn declining_required_policy_never_captures_and_blocks_even_with_no_verify_commit() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--decline")?;
    f.stage()?;
    ensure!(!f.g(&["commit", "--no-verify", "-m", "Declined"])?.success);
    let settings: Value = git::read(&f.checkout().join(".trailvault/settings.local.json"))?;
    ensure!(settings["enabled"] == false && settings["strategy_options"]["push_sessions"] == false);
    let payload = json!({"sessionId":"fixture","cwd":f.checkout(),"prompt":"Must not be captured"});
    let bytes = serde_json::to_vec(&payload)?;
    let hook = git::command(
        &f.home,
        &support::executable()?,
        &["hooks", "copilot-cli", "user-prompt-submitted", "--global"],
        &f.env,
        Some(&bytes),
        Duration::from_secs(30),
    )?;
    ensure!(hook.success, "{}", hook.stderr);
    ensure!(!f.checkout().join(".git/trailvault-sessions").exists());
    Ok(())
}

#[test]
fn declining_optional_policy_allows_commits_and_pushes_without_logging() -> Result<()> {
    let f = Test::new(Some("optional"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--decline")?;
    f.stage()?;
    git::checked(f.g(&["commit", "-m", "No logging requested"])?)?;
    git::checked(f.g(&["push", "origin", "HEAD:refs/heads/optional-test"])?)?;
    ensure!(!f.checkout().join(".git/trailvault-sessions").exists());
    Ok(())
}

#[test]
fn explicit_approval_enables_required_repo_and_revocation_blocks_future_writes() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--approve")?;
    f.stage()?;
    git::checked(f.g(&["commit", "-m", "Approved"])?)?;
    git::checked(f.g(&["push", "origin", "HEAD:refs/heads/approved-test"])?)?;
    git::checked(f.cli(&f.checkout(), &["disable"])?)?;
    f.stage()?;
    ensure!(!f.g(&["commit", "--allow-empty", "-m", "Revoked"])?.success);
    ensure!(
        !f.g(&["push", "origin", "HEAD:refs/heads/revoked-test"])?
            .success
    );
    Ok(())
}

#[test]
fn approval_is_bound_to_committed_policy_and_remote_not_worktree_overrides() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--approve")?;
    fs::remove_file(f.checkout().join(".trailvault/policy.json"))?;
    f.decision("--decline")?;
    f.stage()?;
    ensure!(
        !f.g(&["commit", "-m", "Dirty policy cannot bypass"])?
            .success
    );
    f.decision("--approve")?;
    git::checked(f.g(&[
        "remote",
        "set-url",
        "origin",
        "https://example.invalid/changed",
    ])?)?;
    ensure!(!f.g(&["commit", "-m", "Remote changed"])?.success);
    Ok(())
}

#[test]
fn changed_or_removed_committed_required_policy_needs_review() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--approve")?;
    f.stage()?;
    git::checked(f.g(&[
        "-c",
        "core.hooksPath=/dev/null",
        "rm",
        ".trailvault/policy.json",
    ])?)?;
    git::checked(f.g(&[
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "-m",
        "Changed policy",
    ])?)?;
    ensure!(
        !f.g(&["push", "origin", "HEAD:refs/heads/changed-policy"])?
            .success
    );
    Ok(())
}

#[test]
fn malformed_policy_fails_loudly_and_never_implicitly_enables_publication() -> Result<()> {
    let f = Test::new(None)?;
    put(
        &f.fixture.repo.join(".trailvault/policy.json"),
        "{\"version\":1,\"logging\":\"required\",\"unexpected\":\"no\"}",
    )?;
    f.fixture.g(&["add", ".trailvault/policy.json"])?;
    f.fixture.g(&["commit", "-m", "Invalid policy"])?;
    f.install()?;
    let output = f.clone()?;
    ensure!(
        !output.success && output.stderr.contains("policy"),
        "{}",
        output.stderr
    );
    ensure!(!f.checkout().join(".git/trailvault-sessions").exists());
    Ok(())
}

#[test]
fn installer_refuses_unowned_or_symlinked_gh_and_uninstall_preserves_real_gh() -> Result<()> {
    let f = Test::new(None)?;
    put(&f.shim(), "user's gh\n")?;
    ensure!(
        !f.cli(
            &f.home,
            &[
                "install",
                "--gh-wrapper",
                "--gh-bin",
                support::text(&f.real).as_str()
            ]
        )?
        .success
    );
    ensure!(fs::read_to_string(f.shim())? == "user's gh\n");
    fs::remove_file(f.shim())?;
    std::os::unix::fs::symlink(&f.real, f.shim())?;
    ensure!(
        !f.cli(
            &f.home,
            &[
                "install",
                "--gh-wrapper",
                "--gh-bin",
                support::text(&f.real).as_str()
            ]
        )?
        .success
    );
    fs::remove_file(f.shim())?;
    f.install()?;
    git::checked(f.cli(&f.home, &["uninstall", "--gh-wrapper"])?)?;
    ensure!(!f.shim().exists() && f.real.exists());
    Ok(())
}

#[test]
fn reinstallation_finds_real_gh_even_when_wrapper_is_first_on_path() -> Result<()> {
    let f = Test::new(None)?;
    f.install()?;
    git::checked(f.cli(&f.home, &["install", "--gh-wrapper"])?)?;
    ensure!(f.gh(&["auth", "status"])?.success);
    ensure!(f.recorded()? == ["auth", "status"]);
    Ok(())
}

#[test]
fn clone_derives_default_directory_and_bare_clone_without_policy_still_works() -> Result<()> {
    let mut f = Test::new(None)?;
    f.install()?;
    f.env.insert("GH_FIXTURE_DEST".into(), "project".into());
    ensure!(f.gh(&["repo", "clone", "fixture/project"])?.success);
    ensure!(f.home.join("project/tracked.txt").exists());
    f.env.insert("GH_FIXTURE_DEST".into(), "bare.git".into());
    let output = f.gh(&[
        "repo",
        "clone",
        "fixture/project",
        "bare.git",
        "--",
        "--bare",
    ])?;
    ensure!(output.success, "{}", output.stderr);
    Ok(())
}

#[test]
fn piped_yes_is_not_implicit_consent_and_receipts_are_private_and_untracked() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let f = Test::new(Some("required"))?;
    f.install()?;
    let output = git::command(
        &f.home,
        &f.shim(),
        &["repo", "clone", "fixture/project", "checkout"],
        &f.env,
        Some(b"yes\n"),
        Duration::from_secs(30),
    )?;
    ensure!(!output.success && output.stderr.contains("interactive"));
    let path = f.checkout().join(".git/trailvault-consent.json");
    ensure!(fs::metadata(&path)?.permissions().mode() & 0o777 == 0o600);
    let consent: Value = git::read(&path)?;
    ensure!(consent["decision"] == "pending");
    ensure!(!fs::read_to_string(path)?.contains("user's home"));
    ensure!(git::checked(f.g(&["status", "--porcelain"])?)?.is_empty());
    Ok(())
}

#[test]
fn push_url_and_checkpoint_destination_changes_invalidate_publication_approval() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--approve")?;
    f.stage()?;
    git::checked(f.g(&[
        "remote",
        "set-url",
        "--push",
        "origin",
        "https://example.invalid/other",
    ])?)?;
    ensure!(!f.g(&["commit", "-m", "Destination changed"])?.success);
    Ok(())
}

#[test]
fn optional_source_push_remains_allowed_but_unapproved_checkpoint_target_does_not() -> Result<()> {
    let f = Test::new(Some("optional"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--approve")?;
    let path = f.checkout().join(".trailvault/settings.local.json");
    let mut settings: Value = git::read(&path)?;
    settings["checkpoint_push_remote"] = json!("https://example.invalid/unapproved");
    git::save(&path, &settings)?;
    f.stage()?;
    git::checked(f.g(&["commit", "-m", "Source change"])?)?;
    git::checked(f.g(&["push", "origin", "HEAD:refs/heads/optional-source"])?)?;
    ensure!(!f.cli(&f.checkout(), &["push"])?.success);
    Ok(())
}

fn terminal(f: &Test, answer: &str) -> Result<String> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::{
        io::{Read, Write},
        sync::mpsc,
        thread,
        time::Instant,
    };
    let pair = native_pty_system().openpty(PtySize::default())?;
    let mut command = CommandBuilder::new(f.shim());
    command.args(["repo", "clone", "fixture/project", "checkout"]);
    command.cwd(&f.home);
    for (key, value) in &f.env {
        command.env(key, value);
    }
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if tx.send(Ok(buffer[..count].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if error.raw_os_error() != Some(nix::libc::EIO) {
                        let _ = tx.send(Err(error.to_string()));
                    }
                    break;
                }
            }
        }
    });
    let result = (|| -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut output = String::new();
        let mut answered = false;
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(bytes)) => {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                    if !answered
                        && output.contains("Approve session logging and publication? [y/N]:")
                    {
                        writer.write_all(format!("{answer}\n").as_bytes())?;
                        answered = true;
                    }
                }
                Ok(Err(error)) => anyhow::bail!("Terminal read failed: {error}"),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    anyhow::bail!("Consent prompt timed out: {output}")
                }
            }
        }
        ensure!(answered, "Did not ask for approval: {output}");
        ensure!(child.wait()?.success(), "Clone failed: {output}");
        Ok(output)
    })();
    if result.is_err() {
        child.kill()?;
        child.wait()?;
    }
    drop(writer);
    drop(pair.master);
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("Terminal reader panicked"))?;
    result
}

#[test]
fn normal_gh_clone_prompts_in_a_real_terminal_and_honors_yes_no_and_default_no() -> Result<()> {
    for (mode, answer, approved) in [
        ("required", "yes", true),
        ("required", "no", false),
        ("optional", "", false),
    ] {
        let f = Test::new(Some(mode))?;
        f.install()?;
        let output = terminal(&f, answer)?;
        ensure!(output.contains("origin") && output.contains("Redaction is not a guarantee"));
        let consent: Value = git::read(&f.checkout().join(".git/trailvault-consent.json"))?;
        ensure!(consent["decision"] == if approved { "approved" } else { "declined" });
        f.stage()?;
        ensure!(
            f.g(&["commit", "-m", "Terminal consent"])?.success == (approved || mode == "optional")
        );
    }
    Ok(())
}

#[test]
fn approved_clone_captures_native_session_and_publishes_actual_checkpoint_objects() -> Result<()> {
    let f = Test::new(Some("required"))?;
    f.install()?;
    ensure!(!f.clone()?.success);
    f.decision("--approve")?;
    f.stage()?;
    let id = "40000000-0000-4000-8000-000000000001";
    let transcript = PathBuf::from(&f.env["COPILOT_HOME"])
        .join("session-state")
        .join(id)
        .join("events.jsonl");
    let native = trailvault::redact::jsonl(&[
        json!({"type":"session.start","data":{"sessionId":id}}),
        json!({"type":"user.message","data":{"content":"Synthetic consent-approved change"}}),
        json!({"type":"assistant.message","data":{"content":"Created agent file."}}),
    ])?;
    put(&transcript, &native)?;
    let payload = serde_json::to_vec(
        &json!({"sessionId":id,"cwd":f.checkout(),"transcriptPath":transcript,"prompt":"Synthetic consent-approved change"}),
    )?;
    for event in ["session-start", "user-prompt-submitted", "agent-stop"] {
        if event == "agent-stop" {
            put(&f.checkout().join("agent.txt"), "captured edit\n")?;
        }
        git::checked(git::command(
            &f.home,
            &support::executable()?,
            &["hooks", "copilot-cli", event, "--global"],
            &f.env,
            Some(&payload),
            Duration::from_secs(30),
        )?)?;
    }
    git::checked(f.g(&["add", "agent.txt"])?)?;
    git::checked(f.g(&["commit", "-m", "Approved captured edit"])?)?;
    let message = git::checked(f.g(&["log", "-1", "--format=%B"])?)?;
    let checkpoint = trailvault::capture::trailer(&message, "Trailvault-Checkpoint")
        .context("Missing approved checkpoint")?;
    git::checked(f.g(&["push", "origin", "HEAD:refs/heads/approved-agent-logs"])?)?;
    ensure!(
        f.fixture
            .g(&["for-each-ref", "refs/trailvault/"])?
            .contains(&checkpoint)
    );
    ensure!(fs::read_to_string(transcript)? == native);
    Ok(())
}

#[test]
fn declared_off_policy_never_prompts_or_enables_capture() -> Result<()> {
    let f = Test::new(Some("off"))?;
    f.install()?;
    ensure!(f.clone()?.success);
    let payload = serde_json::to_vec(
        &json!({"sessionId":"fixture","cwd":f.checkout(),"prompt":"Must not be recorded"}),
    )?;
    git::checked(git::command(
        &f.home,
        &support::executable()?,
        &["hooks", "copilot-cli", "user-prompt-submitted", "--global"],
        &f.env,
        Some(&payload),
        Duration::from_secs(30),
    )?)?;
    ensure!(!f.checkout().join(".git/trailvault-sessions").exists());
    ensure!(!f.checkout().join(".git/trailvault-consent.json").exists());
    Ok(())
}

#[test]
fn empty_repository_clones_keep_normal_success_behavior() -> Result<()> {
    let mut f = Test::new(None)?;
    let empty = f.home.join("empty.git");
    git::checked(git::command(
        &f.home,
        Path::new("git"),
        &["init", "--bare", support::text(&empty).as_str()],
        &f.env,
        None,
        Duration::from_secs(30),
    )?)?;
    f.env
        .insert("GH_FIXTURE_REMOTE".into(), support::text(&empty));
    f.install()?;
    let output = f.clone()?;
    ensure!(output.success, "{}", output.stderr);
    ensure!(!f.checkout().join(".git/trailvault-consent.json").exists());
    Ok(())
}
