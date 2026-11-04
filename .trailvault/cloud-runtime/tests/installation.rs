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
use trailvault::{capture, git};

struct Install {
    fixture: Fixture,
    env: BTreeMap<String, String>,
    home: PathBuf,
}

impl Install {
    fn new() -> Result<Self> {
        let mut fixture = Fixture::bare()?;
        let home = fixture.temp.path().join("user home");
        fs::create_dir(&home)?;
        let mut env = fixture.env.clone();
        env.insert("HOME".into(), support::text(&home));
        env.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        fixture.env = env.clone();
        Ok(Self { fixture, env, home })
    }

    fn run(&self, args: &[&str], input: Option<&Value>) -> Result<git::Output> {
        self.run_at(&self.home, args, input)
    }

    fn run_at(&self, cwd: &Path, args: &[&str], input: Option<&Value>) -> Result<git::Output> {
        let binary = support::executable()?;
        let binary_text = support::text(&binary);
        let mut unset_args = vec!["-u", "COPILOT_HOME", binary_text.as_str()];
        unset_args.extend_from_slice(args);
        let (executable, args) = if self.env.contains_key("COPILOT_HOME") {
            (binary.as_path(), args)
        } else {
            (Path::new("/usr/bin/env"), unset_args.as_slice())
        };
        git::command(
            cwd,
            executable,
            args,
            &self.env,
            input.map(serde_json::to_vec).transpose()?.as_deref(),
            Duration::from_secs(30),
        )
    }

    fn ok(&self, args: &[&str]) -> Result<String> {
        git::checked(self.run(args, None)?)
    }

    fn hook_file(&self) -> PathBuf {
        PathBuf::from(&self.env["COPILOT_HOME"]).join("hooks/trailvault.json")
    }

    fn payload(&self) -> Result<Value> {
        let id = "30000000-0000-4000-8000-000000000001";
        let path = PathBuf::from(&self.env["COPILOT_HOME"])
            .join("session-state")
            .join(id)
            .join("events.jsonl");
        let events = vec![
            json!({"type":"session.start","data":{"sessionId":id,"context":{"cwd":self.fixture.repo}}}),
            json!({"type":"user.message","data":{"content":"Synthetic installation test."}}),
        ];
        put(&path, trailvault::redact::jsonl(&events)?)?;
        Ok(
            json!({"sessionId":id,"cwd":self.fixture.repo,"transcriptPath":path,"prompt":"Synthetic installation test."}),
        )
    }

    fn hook(&self, name: &str, payload: &Value) -> Result<()> {
        git::checked(self.run(&["hooks", "copilot-cli", name, "--global"], Some(payload))?)?;
        Ok(())
    }

    fn capture(&self) -> Result<Value> {
        let payload = self.payload()?;
        self.hook("session-start", &payload)?;
        self.hook("user-prompt-submitted", &payload)?;
        put(&self.fixture.repo.join("agent.txt"), "agent edit\n")?;
        self.hook("agent-stop", &payload)?;
        Ok(payload)
    }
}

#[test]
fn installs_outside_git_and_registers_all_events_with_absolute_quoted_binary() -> Result<()> {
    let f = Install::new()?;
    let output = f.ok(&["install"])?;
    ensure!(
        output.contains("locally") && output.contains("publish"),
        "Installation must explain publication defaults"
    );
    let value: Value = git::read(&f.hook_file())?;
    for (event, command) in [
        ("sessionStart", "session-start"),
        ("userPromptSubmitted", "user-prompt-submitted"),
        ("agentStop", "agent-stop"),
        ("sessionEnd", "session-end"),
        ("subagentStop", "subagent-stop"),
        ("preToolUse", "pre-tool-use"),
        ("postToolUse", "post-tool-use"),
        ("errorOccurred", "error-occurred"),
    ] {
        let hook = &value["hooks"][event][0];
        ensure!(hook["type"] == "command");
        let bash = hook["bash"].as_str().context("Missing bash")?;
        ensure!(
            bash.starts_with('\'')
                && bash.contains(
                    support::executable()?
                        .canonicalize()?
                        .to_str()
                        .context("Non-UTF8 binary")?
                )
        );
        ensure!(bash.ends_with(&format!("hooks copilot-cli {command} --global")));
    }
    ensure!(
        !f.fixture.repo.join(".trailvault").exists(),
        "Installing must not enable the current repository"
    );
    Ok(())
}

#[test]
fn entire_style_agent_commands_are_idempotent_and_removable() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["agent", "add", "copilot-cli"])?;
    let first = fs::read(f.hook_file())?;
    f.ok(&["agent", "add", "copilot-cli", "--global"])?;
    ensure!(first == fs::read(f.hook_file())?);
    let list: Value = serde_json::from_str(&f.ok(&["agent", "list"])?)?;
    ensure!(list["copilot-cli"]["installed"] == true);
    f.ok(&["agent", "remove", "copilot-cli"])?;
    f.ok(&["uninstall"])?;
    ensure!(!f.hook_file().exists());
    Ok(())
}

#[test]
fn home_fallback_and_copilot_home_override_are_respected() -> Result<()> {
    let mut f = Install::new()?;
    f.env.remove("COPILOT_HOME");
    f.ok(&["install"])?;
    ensure!(f.home.join(".copilot/hooks/trailvault.json").exists());
    ensure!(
        !f.fixture
            .repo
            .join(".github/hooks/trailvault.json")
            .exists()
    );
    Ok(())
}

#[test]
fn preserves_other_hooks_and_refuses_unowned_or_modified_files() -> Result<()> {
    let f = Install::new()?;
    let sibling = f.hook_file().with_file_name("other.json");
    put(&sibling, b"unrelated file\n")?;
    put(&f.hook_file(), b"{\"version\":1,\"hooks\":{}}\n")?;
    for args in [vec!["install"], vec!["uninstall"]] {
        ensure!(!f.run(&args, None)?.success);
        ensure!(fs::read(f.hook_file())? == b"{\"version\":1,\"hooks\":{}}\n");
    }
    fs::remove_file(f.hook_file())?;
    f.ok(&["install"])?;
    let mut value: Value = git::read(&f.hook_file())?;
    value["hooks"]["sessionStart"][0]["bash"] = json!("echo user-added");
    git::save(&f.hook_file(), &value)?;
    ensure!(!f.run(&["uninstall"], None)?.success);
    ensure!(fs::read(sibling)? == b"unrelated file\n");
    Ok(())
}

#[test]
fn refuses_symlink_destinations_without_touching_targets() -> Result<()> {
    let f = Install::new()?;
    let target = f.home.join("keep.json");
    put(&target, "keep")?;
    fs::create_dir_all(f.hook_file().parent().context("Missing parent")?)?;
    std::os::unix::fs::symlink(&target, f.hook_file())?;
    ensure!(!f.run(&["install"], None)?.success);
    ensure!(!f.run(&["uninstall"], None)?.success);
    ensure!(fs::read(target)? == b"keep");
    Ok(())
}

#[test]
fn unknown_agents_flags_and_help_do_not_mutate_configuration() -> Result<()> {
    let f = Install::new()?;
    for args in [
        vec!["install", "--unknown"],
        vec!["agent", "add", "unknown"],
        vec!["agent", "remove"],
        vec!["hooks", "copilot-cli", "unknown", "--global"],
    ] {
        ensure!(!f.run(&args, Some(&json!({"cwd":f.fixture.repo})))?.success);
    }
    for args in [vec!["install", "--help"], vec!["agent", "--help"]] {
        ensure!(f.run(&args, None)?.success);
    }
    ensure!(!f.hook_file().exists());
    Ok(())
}

#[test]
fn global_hooks_skip_nonrepositories_bare_and_unborn_repositories() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    for (name, args) in [("unborn", vec!["init"]), ("bare", vec!["init", "--bare"])] {
        let path = f.home.join(name);
        fs::create_dir(&path)?;
        support::g(&path, &args)?;
        let result = f.run(
            &["hooks", "copilot-cli", "session-start", "--global"],
            Some(&json!({"cwd":path,"sessionId":"fixture"})),
        )?;
        ensure!(result.success, "{}", result.stderr);
        ensure!(!path.join(".trailvault").exists());
    }
    f.hook(
        "session-start",
        &json!({"cwd":f.home,"sessionId":"fixture"}),
    )?;
    Ok(())
}

#[test]
fn new_repo_captures_locally_and_never_queues_automatic_publication() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    f.capture()?;
    let settings = capture::settings(&git::Repo::discover(&f.fixture.repo)?)?;
    ensure!(settings["enabled"] == true && settings["strategy_options"]["push_sessions"] == false);
    ensure!(
        !f.fixture
            .repo
            .join(".github/hooks/trailvault.json")
            .exists()
    );
    ensure!(!f.fixture.repo.join(".trailvault/settings.json").exists());
    f.fixture.g(&["add", "agent.txt"])?;
    f.fixture.g(&["commit", "-m", "Captured locally"])?;
    ensure!(
        f.fixture
            .g(&["log", "-1", "--format=%B"])?
            .contains("Trailvault-Checkpoint:")
    );
    let remote = f.fixture.remote()?;
    f.fixture.g(&["push", "-u", "origin", "main"])?;
    ensure!(
        f.fixture
            .g(&[
                "--git-dir",
                support::text(&remote).as_str(),
                "for-each-ref",
                "refs/trailvault/"
            ])?
            .is_empty()
    );
    Ok(())
}

#[test]
fn repository_can_opt_out_without_installing_git_hooks() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    put(
        &f.fixture.repo.join(".trailvault/settings.local.json"),
        "{\"enabled\":false}",
    )?;
    let payload = f.payload()?;
    f.hook("session-start", &payload)?;
    ensure!(!f.fixture.repo.join(".git/hooks/post-commit").exists());
    ensure!(!f.fixture.repo.join(".git/trailvault-sessions").exists());
    Ok(())
}

#[test]
fn explicit_repository_publication_opt_in_is_preserved() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    git::checked(f.run_at(&f.fixture.repo, &["enable", "--push-sessions"], None)?)?;
    f.capture()?;
    ensure!(
        capture::settings(&git::Repo::discover(&f.fixture.repo)?)?["strategy_options"]["push_sessions"]
            == true
    );
    Ok(())
}

#[test]
fn preserves_git_hook_chaining_and_repeated_global_sessions() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    let hook = f.fixture.repo.join(".git/hooks/post-commit");
    support::script(&hook, "#!/bin/sh\nprintf ran >> .git/existing-hook-ran\n")?;
    let payload = f.capture()?;
    f.hook("session-start", &payload)?;
    ensure!(
        fs::read_to_string(hook.with_file_name("post-commit.pre-trailvault"))?
            .contains("existing-hook-ran")
    );
    f.fixture.g(&["add", "agent.txt"])?;
    f.fixture.g(&["commit", "-m", "Verify chaining"])?;
    ensure!(fs::read_to_string(f.fixture.repo.join(".git/existing-hook-ran"))? == "ran");
    Ok(())
}

#[test]
fn global_install_removes_only_owned_repository_hooks_on_next_session() -> Result<()> {
    let f = Install::new()?;
    f.fixture.enable()?;
    let sibling = f.fixture.repo.join(".github/hooks/other.json");
    put(&sibling, "{\"version\":1,\"hooks\":{}}")?;
    f.ok(&["install"])?;
    f.hook("session-start", &f.payload()?)?;
    ensure!(
        !f.fixture
            .repo
            .join(".github/hooks/trailvault.json")
            .exists()
    );
    ensure!(sibling.exists());
    Ok(())
}

#[test]
fn enable_after_global_install_does_not_duplicate_agent_hooks_or_publish_implicitly() -> Result<()>
{
    let f = Install::new()?;
    f.ok(&["install"])?;
    git::checked(f.run_at(&f.fixture.repo, &["enable"], None)?)?;
    ensure!(
        !f.fixture
            .repo
            .join(".github/hooks/trailvault.json")
            .exists()
    );
    ensure!(
        capture::settings(&git::Repo::discover(&f.fixture.repo)?)?["strategy_options"]["push_sessions"]
            == false
    );
    Ok(())
}

#[test]
fn external_shared_git_hooks_and_invalid_repo_settings_fail_explicitly() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    let external = f.home.join("shared-hooks");
    fs::create_dir(&external)?;
    f.fixture.g(&[
        "config",
        "core.hooksPath",
        support::text(&external).as_str(),
    ])?;
    let result = f.run(
        &["hooks", "copilot-cli", "session-start", "--global"],
        Some(&f.payload()?),
    )?;
    ensure!(!result.success && result.stderr.contains("shared external"));
    ensure!(fs::read_dir(external)?.next().is_none());
    f.fixture.g(&["config", "--unset", "core.hooksPath"])?;
    put(
        &f.fixture.repo.join(".trailvault/settings.local.json"),
        "{broken",
    )?;
    ensure!(
        !f.run(
            &["hooks", "copilot-cli", "session-start", "--global"],
            Some(&f.payload()?)
        )?
        .success
    );
    Ok(())
}

#[test]
fn global_payload_validation_does_not_create_repo_state() -> Result<()> {
    let f = Install::new()?;
    for input in [
        json!([]),
        json!({"cwd":42}),
        json!({"cwd":f.fixture.repo,"sessionId":"../escape"}),
    ] {
        ensure!(
            !f.run(
                &["hooks", "copilot-cli", "session-start", "--global"],
                Some(&input)
            )?
            .success
        );
    }
    ensure!(!f.fixture.repo.join(".trailvault").exists());
    Ok(())
}

fn subagent_fixture(f: &Install) -> Result<(Value, Value)> {
    f.ok(&["install"])?;
    let payload = f.payload()?;
    f.hook("session-start", &payload)?;
    f.hook("user-prompt-submitted", &payload)?;
    let path = PathBuf::from(payload["transcriptPath"].as_str().context("Missing path")?);
    let mut events = trailvault::redact::parse(&fs::read_to_string(&path)?)?;
    events.extend([
        json!({"type":"assistant.message","data":{"toolRequests":[{"toolCallId":"task-1","name":"task","arguments":{"prompt":"Synthetic child"}}]}}),
        json!({"type":"subagent.started","agentId":"child-1","data":{"toolCallId":"task-1","agentType":"explore","agentName":"explore","agentDescription":"password=synthetic-child-secret"}}),
        json!({"type":"tool.execution_complete","agentId":"child-1","data":{"toolName":"edit","toolCallId":"child-edit","parentToolCallId":"task-1","arguments":{"path":"agent.txt"},"success":true,"result":{"content":"Changed"}}}),
        json!({"type":"tool.execution_complete","data":{"toolCallId":"task-1","success":true,"result":{"content":"Done"}}}),
    ]);
    put(&path, trailvault::redact::jsonl(&events)?)?;
    let mut child = payload.clone();
    child["agentId"] = json!("child-1");
    child["agentName"] = json!("explore");
    Ok((payload, child))
}

#[test]
fn subagent_stop_persists_redacted_task_and_links_parent_without_ending_turn() -> Result<()> {
    let f = Install::new()?;
    let (parent, child) = subagent_fixture(&f)?;
    put(&f.fixture.repo.join("agent.txt"), "child edit\n")?;
    f.hook("subagent-stop", &child)?;
    let before = capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?.remove(0);
    ensure!(before.step_count == 0 && before.phase == "active");
    f.hook("subagent-stop", &child)?;
    f.hook("agent-stop", &parent)?;
    ensure!(
        capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?
            .remove(0)
            .step_count
            == 1
    );
    f.fixture.g(&["add", "agent.txt"])?;
    f.fixture.g(&["commit", "-m", "Subagent checkpoint"])?;
    let id = capture::trailer(
        &f.fixture.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("No checkpoint")?;
    let reference = capture::checkpoint_ref(&id)?;
    let task: Value = serde_json::from_str(
        &f.fixture
            .g(&["show", &format!("{reference}:tasks/task-1/task.json")])?,
    )?;
    ensure!(task["tool_use_id"] == "task-1" && task["agent_id"] == "child-1");
    ensure!(task["subagent_type"] == "explore" && task["completed_at"].is_string());
    ensure!(task["transcript_unavailable_reason"].is_string());
    ensure!(
        !task["task_description"]
            .as_str()
            .context("No description")?
            .contains("synthetic-child-secret")
    );
    let compact = f
        .fixture
        .g(&["show", &format!("{reference}:0/transcript.jsonl")])?;
    ensure!(compact.contains("child-1"));
    let rows = trailvault::redact::parse(&compact)?;
    ensure!(
        rows.iter()
            .any(|row| row["type"] == "tool.execution_complete"
                && row["data"]["toolCallId"] == "task-1"
                && row["data"]["result"]["agent"]["id"] == "child-1")
    );
    let state = capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?.remove(0);
    ensure!(state.checkpoint_count == 1);
    Ok(())
}

#[test]
fn subagent_rejects_missing_ambiguous_mismatched_and_unsafe_identity() -> Result<()> {
    let f = Install::new()?;
    let (parent, child) = subagent_fixture(&f)?;
    for payload in [
        parent.clone(),
        {
            let mut p = child.clone();
            p["agentId"] = json!("not-this-child");
            p
        },
        {
            let mut p = child.clone();
            p["transcriptPath"] = json!(f.home.join("unrelated.jsonl"));
            p
        },
    ] {
        ensure!(
            !f.run(
                &["hooks", "copilot-cli", "subagent-stop", "--global"],
                Some(&payload)
            )?
            .success
        );
    }
    let path = PathBuf::from(parent["transcriptPath"].as_str().context("No path")?);
    let mut events = trailvault::redact::parse(&fs::read_to_string(&path)?)?;
    events.push(json!({"type":"subagent.started","agentId":"child-2","data":{"toolCallId":"task-2","agentName":"explore"}}));
    put(&path, trailvault::redact::jsonl(&events)?)?;
    let mut by_name = parent.clone();
    by_name["agentName"] = json!("explore");
    ensure!(
        !f.run(
            &["hooks", "copilot-cli", "subagent-stop", "--global"],
            Some(&by_name)
        )?
        .success
    );
    let mut unsafe_events = events;
    unsafe_events[3]["data"]["toolCallId"] = json!("../escape");
    put(&path, trailvault::redact::jsonl(&unsafe_events)?)?;
    ensure!(
        !f.run(
            &["hooks", "copilot-cli", "subagent-stop", "--global"],
            Some(&child)
        )?
        .success
    );
    Ok(())
}

#[test]
fn generated_shell_command_executes_with_spaces_and_quotes_in_binary_path() -> Result<()> {
    let f = Install::new()?;
    let binary = f.home.join("binary's directory/trailvault copy");
    fs::create_dir_all(binary.parent().context("No parent")?)?;
    fs::copy(support::executable()?, &binary)?;
    git::checked(git::command(
        &f.home,
        &binary,
        &["install"],
        &f.env,
        None,
        Duration::from_secs(30),
    )?)?;
    let config: Value = git::read(&f.hook_file())?;
    let bash = config["hooks"]["sessionStart"][0]["bash"]
        .as_str()
        .context("Missing bash")?;
    let payload = f.payload()?;
    git::checked(git::command(
        &f.home,
        Path::new("/bin/bash"),
        &["-c", bash],
        &f.env,
        Some(&serde_json::to_vec(&payload)?),
        Duration::from_secs(30),
    )?)?;
    ensure!(f.fixture.repo.join(".git/hooks/post-commit").exists());
    Ok(())
}

#[test]
fn stale_repository_registration_cannot_double_capture_after_global_install() -> Result<()> {
    let f = Install::new()?;
    f.fixture.enable()?;
    f.ok(&["install"])?;
    let payload = f.capture()?;
    git::checked(f.run_at(
        &f.fixture.repo,
        &["hooks", "copilot-cli", "agent-stop"],
        Some(&payload),
    )?)?;
    ensure!(
        capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?
            .remove(0)
            .step_count
            == 1
    );
    Ok(())
}

#[test]
fn refuses_symlinked_hook_directory_without_writing_outside_copilot_home() -> Result<()> {
    let f = Install::new()?;
    let external = f.home.join("external");
    fs::create_dir(&external)?;
    fs::create_dir_all(
        f.hook_file()
            .parent()
            .context("No parent")?
            .parent()
            .context("No home")?,
    )?;
    std::os::unix::fs::symlink(&external, f.hook_file().parent().context("No parent")?)?;
    ensure!(!f.run(&["install"], None)?.success);
    ensure!(fs::read_dir(external)?.next().is_none());
    Ok(())
}

#[test]
fn explicit_publication_opt_in_uploads_real_checkpoint_to_local_remote() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    f.capture()?;
    f.fixture.g(&["add", "agent.txt"])?;
    f.fixture.g(&["commit", "-m", "Locally captured"])?;
    let remote = f.fixture.remote()?;
    git::checked(f.run_at(&f.fixture.repo, &["enable", "--push-sessions"], None)?)?;
    f.fixture.g(&["push", "-u", "origin", "main"])?;
    ensure!(
        !f.fixture
            .g(&[
                "--git-dir",
                support::text(&remote).as_str(),
                "for-each-ref",
                "refs/trailvault/"
            ])?
            .is_empty()
    );
    Ok(())
}

#[test]
fn disabling_and_reenabling_publication_does_not_change_capture_opt_out() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    f.capture()?;
    git::checked(f.run_at(&f.fixture.repo, &["enable", "--push-sessions"], None)?)?;
    git::checked(f.run_at(&f.fixture.repo, &["enable", "--skip-push-sessions"], None)?)?;
    ensure!(
        capture::settings(&git::Repo::discover(&f.fixture.repo)?)?["strategy_options"]["push_sessions"]
            == false
    );
    git::checked(f.run_at(&f.fixture.repo, &["disable"], None)?)?;
    f.hook("session-start", &f.payload()?)?;
    ensure!(capture::settings(&git::Repo::discover(&f.fixture.repo)?)?["enabled"] == false);
    Ok(())
}

#[test]
fn installed_status_detects_malformed_files_and_uninstall_keeps_repository_captures() -> Result<()>
{
    let f = Install::new()?;
    f.ok(&["install"])?;
    f.capture()?;
    f.ok(&["uninstall"])?;
    ensure!(f.fixture.repo.join(".git/hooks/post-commit").exists());
    ensure!(f.fixture.repo.join(".git/trailvault-sessions").exists());
    let status: Value = serde_json::from_str(&f.ok(&["agent", "list"])?)?;
    ensure!(status["copilot-cli"]["installed"] == false);
    put(&f.hook_file(), "{not-json")?;
    ensure!(!f.run(&["agent", "list"], None)?.success);
    Ok(())
}

#[test]
fn global_hook_uses_current_directory_when_cwd_is_absent_and_rejects_relative_cwd() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    let mut payload = f.payload()?;
    payload
        .as_object_mut()
        .context("Not an object")?
        .remove("cwd");
    git::checked(f.run_at(
        &f.fixture.repo,
        &["hooks", "copilot-cli", "session-start", "--global"],
        Some(&payload),
    )?)?;
    ensure!(f.fixture.repo.join(".git/hooks/post-commit").exists());
    payload["cwd"] = json!("relative/path");
    ensure!(
        !f.run(
            &["hooks", "copilot-cli", "session-start", "--global"],
            Some(&payload)
        )?
        .success
    );
    Ok(())
}

#[test]
fn parallel_subagents_remain_distinct_and_duplicate_stops_are_idempotent() -> Result<()> {
    let f = Install::new()?;
    let (parent, child) = subagent_fixture(&f)?;
    let path = PathBuf::from(parent["transcriptPath"].as_str().context("No path")?);
    let mut events = trailvault::redact::parse(&fs::read_to_string(&path)?)?;
    events.push(json!({"type":"subagent.started","agentId":"child-2","data":{"toolCallId":"task-2","agentName":"explore","agentDescription":"Second child"}}));
    put(&path, trailvault::redact::jsonl(&events)?)?;
    f.hook("subagent-stop", &child)?;
    let mut other = child.clone();
    other["agentId"] = json!("child-2");
    f.hook("subagent-stop", &other)?;
    let first = capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?.remove(0);
    f.hook("subagent-stop", &other)?;
    let second = capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?.remove(0);
    ensure!(
        first.tasks.len() == 2
            && serde_json::to_value(first.tasks)? == serde_json::to_value(second.tasks)?
    );
    ensure!(second.step_count == 0 && second.phase == "active");
    Ok(())
}

#[test]
fn subagent_requires_the_exact_parent_transcript_not_another_session_log() -> Result<()> {
    let f = Install::new()?;
    let (_, mut child) = subagent_fixture(&f)?;
    let alternate =
        PathBuf::from(&f.env["COPILOT_HOME"]).join("session-state/other-session/events.jsonl");
    fs::create_dir_all(alternate.parent().context("No parent")?)?;
    fs::copy(
        child["transcriptPath"].as_str().context("No path")?,
        &alternate,
    )?;
    child["transcriptPath"] = json!(alternate);
    let output = f.run(
        &["hooks", "copilot-cli", "subagent-stop", "--global"],
        Some(&child),
    )?;
    ensure!(!output.success && output.stderr.contains("parent session"));
    Ok(())
}

#[test]
fn repository_symlinks_and_unowned_registrations_fail_without_destroying_user_files() -> Result<()>
{
    let f = Install::new()?;
    f.ok(&["install"])?;
    let external = f.home.join("external-hooks");
    fs::create_dir(&external)?;
    fs::create_dir(f.fixture.repo.join(".github"))?;
    std::os::unix::fs::symlink(&external, f.fixture.repo.join(".github/hooks"))?;
    let payload = f.payload()?;
    ensure!(
        !f.run(
            &["hooks", "copilot-cli", "session-start", "--global"],
            Some(&payload)
        )?
        .success
    );
    ensure!(fs::read_dir(&external)?.next().is_none());
    fs::remove_file(f.fixture.repo.join(".github/hooks"))?;
    let registration = f.fixture.repo.join(".github/hooks/trailvault.json");
    put(
        &registration,
        "{\"version\":1,\"hooks\":{\"sessionStart\":[{\"type\":\"command\",\"bash\":\"echo user hook\"}]}}",
    )?;
    ensure!(
        !f.run(
            &["hooks", "copilot-cli", "session-start", "--global"],
            Some(&payload)
        )?
        .success
    );
    ensure!(fs::read_to_string(&registration)?.contains("echo user hook"));
    Ok(())
}

#[test]
fn missing_lifecycle_fields_do_not_bootstrap_repository() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    for (name, payload) in [
        ("session-start", json!({"cwd":f.fixture.repo})),
        (
            "user-prompt-submitted",
            json!({"cwd":f.fixture.repo,"sessionId":"fixture","prompt":42}),
        ),
    ] {
        ensure!(
            !f.run(&["hooks", "copilot-cli", name, "--global"], Some(&payload))?
                .success
        );
        ensure!(!f.fixture.repo.join(".trailvault").exists());
    }
    Ok(())
}

#[test]
fn installation_and_repository_settings_are_private_and_reinstallation_can_move_binary()
-> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let f = Install::new()?;
    f.ok(&["install"])?;
    ensure!(fs::metadata(f.hook_file())?.permissions().mode() & 0o777 == 0o600);
    f.capture()?;
    ensure!(
        fs::metadata(f.fixture.repo.join(".trailvault/settings.local.json"))?
            .permissions()
            .mode()
            & 0o777
            == 0o600
    );
    let binary = f.home.join("moved-trailvault");
    fs::copy(support::executable()?, &binary)?;
    git::checked(git::command(
        &f.home,
        &binary,
        &["install"],
        &f.env,
        None,
        Duration::from_secs(30),
    )?)?;
    ensure!(fs::read_to_string(f.hook_file())?.contains("moved-trailvault"));
    Ok(())
}

#[test]
fn concurrent_installations_leave_one_valid_registration() -> Result<()> {
    let f = Install::new()?;
    std::thread::scope(|scope| -> Result<()> {
        let workers: Vec<_> = (0..4).map(|_| scope.spawn(|| f.ok(&["install"]))).collect();
        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Installer worker panicked"))??;
        }
        Ok(())
    })?;
    let value: Value = git::read(&f.hook_file())?;
    ensure!(value["hooks"].as_object().context("No hooks")?.len() == 8);
    ensure!(
        value["hooks"]
            .as_object()
            .context("No hooks")?
            .values()
            .all(|entries| entries.as_array().is_some_and(|entries| entries.len() == 1))
    );
    Ok(())
}

#[test]
fn global_hooks_initialize_the_correct_worktree_from_a_nested_directory() -> Result<()> {
    let f = Install::new()?;
    f.ok(&["install"])?;
    let worktree = f.home.join("linked-worktree");
    f.fixture.g(&[
        "worktree",
        "add",
        "-b",
        "other",
        support::text(&worktree).as_str(),
    ])?;
    let nested = worktree.join("nested");
    fs::create_dir(&nested)?;
    let mut payload = f.payload()?;
    payload["cwd"] = json!(nested);
    f.hook("session-start", &payload)?;
    ensure!(worktree.join(".trailvault/settings.local.json").exists());
    ensure!(
        !f.fixture
            .repo
            .join(".trailvault/settings.local.json")
            .exists()
    );
    let states = capture::sessions(&git::Repo::discover(&worktree)?)?;
    ensure!(states.len() == 1 && states[0].worktree_path == worktree.canonicalize()?);
    ensure!(capture::sessions(&git::Repo::discover(&f.fixture.repo)?)?.is_empty());
    Ok(())
}

#[test]
fn subagent_task_survives_prepared_commit_recovery_without_replaying_child_stop() -> Result<()> {
    let f = Install::new()?;
    let (parent, child) = subagent_fixture(&f)?;
    put(&f.fixture.repo.join("agent.txt"), "child edit\n")?;
    f.hook("subagent-stop", &child)?;
    f.hook("agent-stop", &parent)?;
    f.fixture.g(&["add", "agent.txt"])?;
    let message = f.fixture.repo.join(".git/recovery-message");
    put(&message, "Recover child task\n")?;
    git::checked(f.run_at(
        &f.fixture.repo,
        &[
            "hooks",
            "git",
            "prepare-commit-msg",
            support::text(&message).as_str(),
            "message",
        ],
        None,
    )?)?;
    f.fixture.g(&[
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "-F",
        support::text(&message).as_str(),
    ])?;
    ensure!(
        f.fixture
            .g(&["for-each-ref", "refs/trailvault/"])?
            .is_empty()
    );
    git::checked(f.run_at(&f.fixture.repo, &["recover"], None)?)?;
    git::checked(f.run_at(&f.fixture.repo, &["recover"], None)?)?;
    let id = capture::trailer(
        &f.fixture.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("No checkpoint")?;
    let reference = capture::checkpoint_ref(&id)?;
    let task: Value = serde_json::from_str(
        &f.fixture
            .g(&["show", &format!("{reference}:tasks/task-1/task.json")])?,
    )?;
    ensure!(task["agent_id"] == "child-1");
    Ok(())
}
