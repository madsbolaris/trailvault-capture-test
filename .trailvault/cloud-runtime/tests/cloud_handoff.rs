mod support;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};
use support::Fixture;
use trailvault::{
    capture,
    cloud::{self, Artifact},
    git,
};

struct Handoff {
    f: Fixture,
    artifact: std::path::PathBuf,
    reference: String,
}

impl Handoff {
    fn new() -> Result<Self> {
        let f = Fixture::new()?;
        f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":true}}))?;
        f.remote()?;
        let (_, reference) = f.commit_capture()?;
        f.g(&[
            "-c",
            "core.hooksPath=/dev/null",
            "push",
            "origin",
            "HEAD:main",
        ])?;
        let artifact = f.temp.path().join("checkpoints.json");
        cloud::export(
            &git::Repo::discover(&f.repo)?,
            "owner/repo",
            123,
            1,
            &artifact,
        )?;
        Ok(Self {
            f,
            artifact,
            reference,
        })
    }
    fn read(&self) -> Result<Artifact> {
        Ok(serde_json::from_slice(&fs::read(&self.artifact)?)?)
    }
    fn write(&self, artifact: &Artifact) -> Result<()> {
        git::save(&self.artifact, artifact)
    }
    fn publish(&self) -> Result<()> {
        cloud::publish(
            &git::Repo::discover(&self.f.repo)?,
            &self.artifact,
            "owner/repo",
            123,
            1,
            "main",
        )
    }
}

#[test]
fn export_publish_and_fresh_clone_preserve_exact_native_bytes() -> Result<()> {
    let h = Handoff::new()?;
    let artifact = h.read()?;
    ensure!(artifact.checkpoints.len() == 1);
    let native = artifact.checkpoints[0]
        .files
        .get("0/full.jsonl")
        .context("Missing native transcript")?;
    let compact = trailvault::redact::parse(&artifact.checkpoints[0].files["0/transcript.jsonl"])?;
    ensure!(compact[0]["version"] == 2 && compact[0]["classification_version"] == 1);
    ensure!(
        compact
            .iter()
            .skip(1)
            .all(|row| row["classification"]["version"] == 1)
    );
    h.publish()?;
    let fresh = h.f.temp.path().join("fresh");
    support::g(
        h.f.temp.path(),
        &[
            "clone",
            &support::text(&h.f.temp.path().join("remote.git")),
            &support::text(&fresh),
        ],
    )?;
    support::g(
        &fresh,
        &[
            "fetch",
            "origin",
            &format!("{}:{}", h.reference, h.reference),
        ],
    )?;
    let raw = git::run(
        &fresh,
        &["show", &format!("{}:0/full.jsonl", h.reference)],
        &h.f.env,
        None,
    )?;
    ensure!(raw.success && raw.stdout == *native);
    ensure!(
        support::g(&fresh, &["log", "-1", "--format=%B"])?.contains(&artifact.checkpoints[0].id)
    );
    h.publish()?;
    let existing = h.f.g(&["ls-remote", "origin", &h.reference])?;
    h.publish()?;
    ensure!(h.f.g(&["ls-remote", "origin", &h.reference])? == existing);
    Ok(())
}

#[test]
fn rejects_unknown_fields_and_workflow_identity_substitution() -> Result<()> {
    let h = Handoff::new()?;
    let mut raw: Value = serde_json::from_slice(&fs::read(&h.artifact)?)?;
    raw["execute"] = json!("NEVER_RUN");
    git::save(&h.artifact, &raw)?;
    ensure!(h.publish().is_err());
    raw.as_object_mut()
        .context("Missing object")?
        .remove("execute");
    raw["run_id"] = json!(999);
    git::save(&h.artifact, &raw)?;
    ensure!(h.publish().is_err());
    raw["run_id"] = json!(123);
    raw["repository"] = json!("attacker/repo");
    git::save(&h.artifact, &raw)?;
    ensure!(h.publish().is_err());
    ensure!(h.f.g(&["ls-remote", "origin", &h.reference])?.is_empty());
    Ok(())
}

#[test]
fn rejects_path_injection_hash_changes_and_native_identity_changes() -> Result<()> {
    let h = Handoff::new()?;
    let original = h.read()?;
    for path in [
        "../outside",
        ".git/config",
        ".github/workflows/run.yml",
        "0/../../outside",
        "tasks/../task.json",
        "0/hooks.sh",
    ] {
        let mut modified = h.read()?;
        modified.checkpoints[0]
            .files
            .insert(path.into(), "NEVER_EXECUTE".into());
        h.write(&modified)?;
        ensure!(h.publish().is_err(), "Accepted unsafe path {path}");
        h.write(&original)?;
    }
    let mut modified = h.read()?;
    modified.checkpoints[0].files.insert(
        "0/full.jsonl".into(),
        "{\"type\":\"assistant.message\"}\n".into(),
    );
    h.write(&modified)?;
    ensure!(h.publish().is_err());
    ensure!(h.f.g(&["ls-remote", "origin", &h.reference])?.is_empty());
    Ok(())
}

#[test]
fn rejects_unlinked_source_or_invented_file_attribution_before_publishing() -> Result<()> {
    let h = Handoff::new()?;
    let original = h.read()?;
    let mut changed = h.read()?;
    changed.checkpoints[0].source_commit = h.f.g(&["rev-parse", "HEAD~1"])?;
    h.write(&changed)?;
    ensure!(h.publish().is_err());
    let mut changed = original;
    let root = changed.checkpoints[0]
        .files
        .get_mut("metadata.json")
        .context("Missing root")?;
    let mut json: Value = serde_json::from_str(root)?;
    json["files_touched"] = json!(["unrelated-secret.txt"]);
    *root = serde_json::to_string(&json)?;
    h.write(&changed)?;
    ensure!(h.publish().is_err());
    ensure!(h.f.g(&["ls-remote", "origin", &h.reference])?.is_empty());
    Ok(())
}

#[test]
fn validates_whole_artifact_before_any_ref_is_published() -> Result<()> {
    let h = Handoff::new()?;
    let mut artifact = h.read()?;
    let mut invalid = cloud::Checkpoint {
        id: capture::checkpoint_id(),
        source_commit: artifact.checkpoints[0].source_commit.clone(),
        files: artifact.checkpoints[0].files.clone(),
    };
    invalid
        .files
        .insert(".git/config".into(), "NEVER_EXECUTE".into());
    artifact.checkpoints.push(invalid);
    h.write(&artifact)?;
    ensure!(h.publish().is_err());
    ensure!(h.f.g(&["ls-remote", "origin", &h.reference])?.is_empty());
    Ok(())
}

#[test]
fn export_refuses_local_only_publication() -> Result<()> {
    let h = Handoff::new()?;
    h.f.configure(&json!({"enabled":true,"strategy_options":{"push_sessions":false}}))?;
    ensure!(
        cloud::export(
            &git::Repo::discover(&h.f.repo)?,
            "owner/repo",
            123,
            1,
            Path::new("/unused/checkpoints.json")
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn publisher_dispatch_pins_trusted_code_and_checks_the_completed_source() {
    let workflow = include_str!("../.github/workflows/trailvault-cloud-publish.yml");
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(!workflow.contains("workflow_run:"));
    assert!(workflow.contains(".path == \"dynamic/copilot-swe-agent/copilot\""));
    assert!(workflow.contains(".head_repository.full_name == $repo"));
    assert!(workflow.contains(".run_attempt == $attempt"));
    assert!(workflow.contains("ref: ${{ github.event.repository.default_branch }}"));
    assert!(workflow.contains("persist-credentials: false"));
}

#[test]
fn token_dispatch_uses_stdin_not_arguments_and_reports_http_refusals() -> Result<()> {
    let mut f = Fixture::bare()?;
    let manifest = f.temp.path().join("checkpoints.json");
    git::save(
        &manifest,
        &json!({"version":1,"repository":"owner/repo","run_id":123,
        "run_attempt":2,"source_branch":"copilot/test","checkpoints":[]}),
    )?;
    let tools = f.temp.path().join("tools");
    support::script(
        &tools.join("curl"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGUMENTS\"\ncat > \"$CONFIGURATION\"\nprintf '%s' \"$MOCK_STATUS\"\n",
    )?;
    let configuration = f.temp.path().join("curl-config");
    let arguments = f.temp.path().join("curl-arguments");
    f.env.extend([
        (
            "PATH".into(),
            format!("{}:{}", tools.display(), f.env["PATH"]),
        ),
        (
            "TRAILVAULT_DISPATCH_TOKEN".into(),
            "fictional-test-token".into(),
        ),
        ("GITHUB_REPOSITORY".into(), "owner/repo".into()),
        ("GITHUB_RUN_ID".into(), "123".into()),
        ("GITHUB_RUN_ATTEMPT".into(), "2".into()),
        ("MOCK_STATUS".into(), "204".into()),
        ("CONFIGURATION".into(), support::text(&configuration)),
        ("ARGUMENTS".into(), support::text(&arguments)),
    ]);
    let output = f.cli(&["cloud", "dispatch", &support::text(&manifest)])?;
    ensure!(output.contains("Dispatched trusted publisher for run 123/2"));
    ensure!(!output.contains("fictional-test-token"));
    let config = fs::read_to_string(&configuration)?;
    ensure!(config.contains("Authorization: Bearer fictional-test-token"));
    ensure!(
        config.contains(
            "/repos/owner/repo/actions/workflows/trailvault-cloud-publish.yml/dispatches"
        )
    );
    ensure!(!fs::read_to_string(&arguments)?.contains("fictional-test-token"));
    f.env.insert("MOCK_STATUS".into(), "403".into());
    let error = f
        .cli(&["cloud", "dispatch", &support::text(&manifest)])
        .err()
        .context("HTTP refusal was silently accepted")?;
    let message = format!("{error:#}");
    ensure!(message.contains("403") && !message.contains("fictional-test-token"));
    let retained = f.repo.join(".git/trailvault-dispatch-token");
    git::atomic(&retained, b"fictional-retained-token")?;
    let output = std::process::Command::new(support::executable()?)
        .args(["cloud", "dispatch", &support::text(&manifest)])
        .current_dir(&f.repo)
        .envs(&f.env)
        .env_remove("TRAILVAULT_DISPATCH_TOKEN")
        .env("MOCK_STATUS", "204")
        .output()?;
    ensure!(
        output.status.success(),
        "Retained-credential dispatch failed"
    );
    ensure!(
        !retained.exists(),
        "Dispatch must delete the private credential"
    );
    ensure!(
        fs::read_to_string(&configuration)?
            .contains("Authorization: Bearer fictional-retained-token")
    );
    fs::remove_file(&configuration)?;
    f.env.insert("GITHUB_RUN_ATTEMPT".into(), "3".into());
    ensure!(
        f.cli(&["cloud", "dispatch", &support::text(&manifest)])
            .is_err()
    );
    ensure!(
        !configuration.exists(),
        "Mismatched run attempted an HTTP call"
    );
    Ok(())
}
