mod support;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use support::{Fixture, event, put, script};
use trailvault::{capture, git, redact};
use uuid::Uuid;

struct Session {
    f: Fixture,
    id: String,
    payload: Value,
    native: PathBuf,
}

impl Session {
    fn new(with_native: bool) -> Result<Self> {
        let f = Fixture::new()?;
        f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"publish_on_tool_completion":true,"push_sessions":false}}))?;
        let id = Uuid::new_v4().to_string();
        let native = PathBuf::from(&f.env["COPILOT_HOME"])
            .join("session-state")
            .join(&id)
            .join("events.jsonl");
        if with_native {
            put(
                &native,
                redact::jsonl(&[
                    event(
                        "session.start",
                        json!({"sessionId":id,"version":1,"producer":"copilot-agent","context":{"cwd":f.repo}}),
                    ),
                    event(
                        "user.message",
                        json!({"content":"Fictional cloud ordering reproduction."}),
                    ),
                    event(
                        "assistant.message",
                        json!({"content":"I will edit one file."}),
                    ),
                ])?,
            )?;
        }
        // The observed cloud payloads did not advertise transcriptPath.
        let payload = json!({"sessionId":id,"cwd":f.repo,"prompt":"Fictional cloud ordering reproduction.","source":"new"});
        // Preserve the actual observed cloud order.
        f.hook("user-prompt-submitted", &payload)?;
        f.hook("session-start", &payload)?;
        Ok(Self {
            f,
            id,
            payload,
            native,
        })
    }

    fn edit_with_tool_hooks(&self) -> Result<()> {
        self.f.hook("pre-tool-use", &self.payload)?;
        put(
            &self.f.repo.join("cloud-edit.txt"),
            "Fictional hosted edit\n",
        )?;
        self.f.hook("post-tool-use", &self.payload)?;
        Ok(())
    }
    fn state(&self) -> Result<capture::State> {
        git::read(
            &self
                .f
                .repo
                .join(".git/trailvault-sessions")
                .join(format!("{}.json", self.id)),
        )
    }
    fn commit(&self) -> Result<String> {
        self.f.g(&["add", "cloud-edit.txt"])?;
        self.f.g(&[
            "commit",
            "-m",
            "Simulate hosted mid-run report_progress commit",
        ])?;
        self.f.g(&["rev-parse", "HEAD"])
    }
}

#[test]
fn missing_native_transcript_is_not_successful_tool_capture() -> Result<()> {
    let session = Session::new(false)?;
    let error = session
        .f
        .hook("pre-tool-use", &session.payload)
        .err()
        .context("Tool capture silently accepted a missing native transcript")?;
    ensure!(format!("{error:#}").contains("No such file"), "{error:#}");
    let state = session.state()?;
    ensure!(state.prompts.len() == 1 && state.phase == "active");
    ensure!(state.step_count == 0 && state.files_touched.is_empty());
    ensure!(state.transcript_path.is_none() && state.pending_transcript.is_none());
    ensure!(!session.native.exists());
    let error = session
        .f
        .hook("agent-stop", &session.payload)
        .err()
        .context("Missing transcript was silently accepted")?;
    ensure!(format!("{error:#}").contains("No such file"), "{error:#}");
    Ok(())
}

#[test]
fn hosted_commit_before_stop_links_and_publishes_native_context() -> Result<()> {
    let session = Session::new(true)?;
    let remote = session.f.remote()?;
    session.f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":true,"publish_on_tool_completion":true}}))?;
    session.edit_with_tool_hooks()?;
    ensure!(session.state()?.phase == "active" && session.state()?.step_count == 0);
    let source = session.commit()?;
    let message = session.f.g(&["log", "-1", "--format=%B"])?;
    let checkpoint = capture::trailer(&message, "Trailvault-Checkpoint")
        .context("A hosted mid-turn commit must link without an agent-stop")?;
    let reference = capture::checkpoint_ref(&checkpoint)?;
    session.f.g(&["push", "origin", "HEAD:main"])?;
    let fresh = session.f.temp.path().join("fresh");
    support::g(
        session.f.temp.path(),
        &["clone", &support::text(&remote), &support::text(&fresh)],
    )?;
    support::g(
        &fresh,
        &["fetch", "origin", &format!("{reference}:{reference}")],
    )?;
    let published = support::g(&fresh, &["show", &format!("{reference}:0/full.jsonl")])?;
    ensure!(published == fs::read_to_string(&session.native)?.trim_end());
    session.f.hook("agent-stop", &session.payload)?;
    let state = session.state()?;
    ensure!(state.pending_transcript.is_some() && state.files_touched.is_empty());
    session.f.cli(&["recover"])?;
    ensure!(
        session.f.g(&["rev-parse", "HEAD"])? == source,
        "A late stop must not rewrite source history"
    );
    ensure!(session.f.g(&["rev-list", "--count", &reference])? == "1");
    Ok(())
}

#[test]
fn session_start_chains_hooks_when_host_changes_hooks_path_after_setup() -> Result<()> {
    let session = Session::new(true)?;
    let hooks = session.f.repo.join(".git/copilot-hooks");
    let previous = hooks.join("prepare-commit-msg");
    script(
        &previous,
        "#!/bin/sh\nprintf 'chained\\n' >> .git/host-hook-called\n",
    )?;
    session
        .f
        .g(&["config", "core.hooksPath", &support::text(&hooks)])?;
    session.f.hook("session-start", &session.payload)?;
    session.f.hook("session-start", &session.payload)?;
    session.edit_with_tool_hooks()?;
    session.commit()?;
    ensure!(
        capture::trailer(
            &session.f.g(&["log", "-1", "--format=%B"])?,
            "Trailvault-Checkpoint"
        )
        .is_some()
    );
    ensure!(fs::read_to_string(session.f.repo.join(".git/host-hook-called"))? == "chained\n");
    ensure!(
        fs::read_to_string(hooks.join("prepare-commit-msg.pre-trailvault"))?
            .contains("host-hook-called")
    );
    Ok(())
}

#[test]
fn each_mid_turn_commit_gets_fresh_context_without_waiting_for_stop() -> Result<()> {
    let session = Session::new(true)?;
    session.edit_with_tool_hooks()?;
    session.commit()?;
    let first = capture::trailer(
        &session.f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("Missing first checkpoint")?;
    let mut events = redact::parse(&fs::read_to_string(&session.native)?)?;
    events.push(event(
        "assistant.message",
        json!({"content":"A second genuine fixture reply."}),
    ));
    put(&session.native, redact::jsonl(&events)?)?;
    session.f.hook("pre-tool-use", &session.payload)?;
    put(
        &session.f.repo.join("cloud-edit.txt"),
        "Second edit in the same turn\n",
    )?;
    session.f.hook("post-tool-use", &session.payload)?;
    session.commit()?;
    let second = capture::trailer(
        &session.f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("Missing second checkpoint")?;
    ensure!(first != second);
    ensure!(session.state()?.checkpoint_count == 2 && session.state()?.phase == "active");
    let latest = session.f.g(&[
        "show",
        &format!("{}:0/full.jsonl", capture::checkpoint_ref(&second)?),
    ])?;
    ensure!(latest.contains("A second genuine fixture reply."));
    let earlier = session.f.g(&[
        "show",
        &format!("{}:0/full.jsonl", capture::checkpoint_ref(&first)?),
    ])?;
    ensure!(!earlier.contains("A second genuine fixture reply."));
    Ok(())
}

#[test]
fn hosted_tool_completion_recovers_and_uploads_when_post_commit_and_pre_push_are_skipped()
-> Result<()> {
    let session = Session::new(true)?;
    let remote = session.f.remote()?;
    session.f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":true,"publish_on_tool_completion":true}}))?;
    session.edit_with_tool_hooks()?;
    script(
        &session.f.repo.join(".git/hooks/post-commit"),
        "#!/bin/sh\nexit 0\n",
    )?;
    script(
        &session.f.repo.join(".git/hooks/pre-push"),
        "#!/bin/sh\nexit 0\n",
    )?;
    session.commit()?;
    let checkpoint = capture::trailer(
        &session.f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("Missing prepared trailer")?;
    let reference = capture::checkpoint_ref(&checkpoint)?;
    ensure!(session.f.repo.join(".git/trailvault-commit.json").exists());
    ensure!(
        session
            .f
            .g(&["for-each-ref", "refs/trailvault/checkpoints"])?
            .is_empty()
    );
    let prepared = fs::read_to_string(&session.native)?;
    session.f.g(&["push", "origin", "HEAD:main"])?;
    let mut events = redact::parse(&prepared)?;
    events.push(event(
        "assistant.message",
        json!({"content":"After the hosted source commit."}),
    ));
    put(&session.native, redact::jsonl(&events)?)?;
    let mut payload = session.payload.clone();
    payload["toolName"] = json!("provider-qualified-progress-tool");
    session.f.hook("post-tool-use", &payload)?;
    ensure!(!session.f.repo.join(".git/trailvault-commit.json").exists());
    ensure!(session.state()?.checkpoint_count == 1);
    let published = support::g(
        session.f.temp.path(),
        &[
            "--git-dir",
            &support::text(&remote),
            "show",
            &format!("{reference}:0/full.jsonl"),
        ],
    )?;
    ensure!(
        published == prepared.trim_end(),
        "Recovery must finalize the immutable prepared snapshot before reading newer native events"
    );
    ensure!(
        fs::read_to_string(
            session
                .state()?
                .pending_transcript
                .context("Missing updated pending snapshot")?
        )?
        .contains("After the hosted source commit.")
    );
    let queue: Vec<String> = git::read(&session.f.repo.join(".git/trailvault/push-queue.json"))?;
    ensure!(queue.is_empty());
    session.f.hook("post-tool-use", &payload)?;
    ensure!(
        session.state()?.checkpoint_count == 1,
        "Repeated tool completion must not duplicate checkpoints"
    );
    Ok(())
}

#[test]
fn hosted_tool_completion_respects_local_only_publication() -> Result<()> {
    let session = Session::new(true)?;
    let remote = session.f.remote()?;
    session.f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":false,"publish_on_tool_completion":true}}))?;
    session.edit_with_tool_hooks()?;
    session.commit()?;
    let checkpoint = capture::trailer(
        &session.f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("Missing checkpoint")?;
    let reference = capture::checkpoint_ref(&checkpoint)?;
    session.f.g(&["push", "origin", "HEAD:main"])?;
    let mut payload = session.payload.clone();
    payload["toolName"] = json!("report_progress");
    session.f.hook("post-tool-use", &payload)?;
    let queue: Vec<String> = git::read(&session.f.repo.join(".git/trailvault/push-queue.json"))?;
    ensure!(queue.contains(&reference));
    ensure!(
        !git::run(
            session.f.temp.path(),
            &[
                "--git-dir",
                &support::text(&remote),
                "rev-parse",
                "--verify",
                &reference
            ],
            &session.f.env,
            None
        )?
        .success
    );
    Ok(())
}

#[test]
fn ordinary_tool_completion_does_not_publish_before_a_source_push() -> Result<()> {
    let session = Session::new(true)?;
    let remote = session.f.remote()?;
    session.f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":true}}))?;
    session.edit_with_tool_hooks()?;
    session.f.hook("agent-stop", &session.payload)?;
    session.commit()?;
    session.f.hook("post-tool-use", &session.payload)?;
    let queue: Vec<String> = git::read(&session.f.repo.join(".git/trailvault/push-queue.json"))?;
    ensure!(queue.len() == 1);
    ensure!(
        !git::run(
            session.f.temp.path(),
            &[
                "--git-dir",
                &support::text(&remote),
                "rev-parse",
                "--verify",
                &queue[0]
            ],
            &session.f.env,
            None
        )?
        .success
    );
    Ok(())
}

#[test]
fn enabling_cloud_mode_does_not_override_local_only_publication() -> Result<()> {
    let session = Session::new(true)?;
    session.f.cli(&["enable", "--skip-push-sessions"])?;
    session.f.cli(&["enable", "--cloud"])?;
    let settings = git::read::<Value>(&session.f.repo.join(".trailvault/settings.local.json"))?;
    ensure!(settings["strategy_options"]["publish_on_tool_completion"] == true);
    ensure!(settings["strategy_options"]["cloud_artifact"] == true);
    ensure!(settings["strategy_options"]["push_sessions"] == false);
    Ok(())
}

#[test]
fn ordinary_local_tool_hooks_do_not_require_cloud_session_metadata_or_transcripts() -> Result<()> {
    let mut f = Fixture::new()?;
    f.env.insert(
        "TRAILVAULT_DISPATCH_TOKEN".into(),
        "fictional-unused-token".into(),
    );
    let payload = json!({"cwd":f.repo,"toolName":"read_file"});
    f.hook("pre-tool-use", &payload)?;
    f.hook("post-tool-use", &payload)?;
    ensure!(!f.repo.join(".git/trailvault-sessions").exists());
    ensure!(!f.repo.join(".git/trailvault-dispatch-token").exists());
    ensure!(
        !PathBuf::from(&f.env["COPILOT_HOME"])
            .join("session-state")
            .exists()
    );
    Ok(())
}

#[test]
fn control_stop_before_commit_can_link_the_same_native_context() -> Result<()> {
    let session = Session::new(true)?;
    session.edit_with_tool_hooks()?;
    let original = fs::read(&session.native)?;
    session.f.hook("agent-stop", &session.payload)?;
    session.commit()?;
    let message = session.f.g(&["log", "-1", "--format=%B"])?;
    let checkpoint =
        capture::trailer(&message, "Trailvault-Checkpoint").context("Control case did not link")?;
    let reference = capture::checkpoint_ref(&checkpoint)?;
    ensure!(
        !session
            .f
            .g(&["show", &format!("{reference}:0/full.jsonl")])?
            .is_empty()
    );
    ensure!(fs::read(&session.native)? == original);
    Ok(())
}

#[test]
fn cloud_artifact_mode_retains_checkpoints_for_post_job_export_without_direct_push() -> Result<()> {
    let mut session = Session::new(true)?;
    session.f.remote()?;
    session.f.configure(
        &json!({"enabled":true,"commit_linking":"always","strategy_options":{
            "push_sessions":true,"publish_on_tool_completion":true,"cloud_artifact":true
        }}),
    )?;
    // This fixture switches adapters after starting with ordinary CLI defaults.
    session.payload["client"] = json!({"app":"copilot","environment":"cloud"});
    session.edit_with_tool_hooks()?;
    session.commit()?;
    let id = capture::trailer(
        &session.f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("Missing checkpoint trailer")?;
    let reference = capture::checkpoint_ref(&id)?;
    session.f.g(&["push", "origin", "HEAD:main"])?;
    session.f.hook("post-tool-use", &session.payload)?;
    ensure!(
        session
            .f
            .g(&["ls-remote", "origin", &reference])?
            .is_empty(),
        "Artifact mode must not push refs from either tool or pre-push hooks"
    );
    let queue: Vec<String> = git::read(&session.f.repo.join(".git/trailvault/push-queue.json"))?;
    ensure!(queue.as_slice() == std::slice::from_ref(&reference));
    let manifest = session.f.temp.path().join("checkpoints.json");
    trailvault::cloud::export(
        &git::Repo::discover(&session.f.repo)?,
        "owner/repo",
        123,
        1,
        &manifest,
    )?;
    let artifact: trailvault::cloud::Artifact = git::read(&manifest)?;
    ensure!(artifact.checkpoints.len() == 1 && artifact.checkpoints[0].id == id);
    session.f.cli(&["push"])?;
    ensure!(
        !session
            .f
            .g(&["ls-remote", "origin", &reference])?
            .is_empty(),
        "Explicit publication must remain available outside the hosted restricted environment"
    );
    Ok(())
}

#[test]
fn agent_step_dispatch_credential_is_private_and_never_part_of_checkpoint_export() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut session = Session::new(true)?;
    session.f.remote()?;
    session.f.configure(
        &json!({"enabled":true,"commit_linking":"always","strategy_options":{
            "push_sessions":true,"publish_on_tool_completion":true,"cloud_artifact":true
        }}),
    )?;
    session.payload["client"] = json!({"app":"copilot","environment":"cloud"});
    session.f.env.insert(
        "TRAILVAULT_DISPATCH_TOKEN".into(),
        "fictional-retained-token".into(),
    );
    session.f.hook("session-start", &session.payload)?;
    let retained = session.f.repo.join(".git/trailvault-dispatch-token");
    ensure!(fs::read_to_string(&retained)? == "fictional-retained-token");
    ensure!(fs::metadata(&retained)?.permissions().mode() & 0o777 == 0o600);
    session.edit_with_tool_hooks()?;
    session.commit()?;
    let manifest = session.f.temp.path().join("checkpoints.json");
    trailvault::cloud::export(
        &git::Repo::discover(&session.f.repo)?,
        "owner/repo",
        123,
        1,
        &manifest,
    )?;
    ensure!(!fs::read_to_string(&manifest)?.contains("fictional-retained-token"));
    fs::remove_file(&retained)?;
    session
        .f
        .configure(&json!({"enabled":true,"strategy_options":{
            "push_sessions":false,"cloud_artifact":true
        }}))?;
    session.f.hook("session-start", &session.payload)?;
    ensure!(
        !retained.exists(),
        "Local-only sessions must not retain publication credentials"
    );
    Ok(())
}
