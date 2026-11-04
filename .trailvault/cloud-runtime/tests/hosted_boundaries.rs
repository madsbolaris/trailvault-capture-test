mod support;

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::fs;
use support::{Fixture, put};
use trailvault::{capture, git};

fn fixture() -> Result<(Fixture, Value)> {
    let f = Fixture::new()?;
    f.g(&[
        "remote",
        "add",
        "origin",
        "https://github.com/madsbolaris/trailvault-capture-test.git",
    ])?;
    let id = uuid::Uuid::new_v4().to_string();
    let path = f.temp.path().join("handoff.jsonl");
    put(
        &path,
        format!(
            "{}\n",
            json!({"type":"user","sessionId":id,"cwd":"/home/user/repo","message":{"content":"synthetic hosted boundary"}})
        ),
    )?;
    let payload = json!({"native_session_id":id,"transcript_path":path,"provenance":{
        "remote_session_id":"session_SyntheticBoundary12345","repository":"madsbolaris/trailvault-capture-test",
        "source_commit":f.g(&["rev-parse","HEAD"])?,"source_branch":"test/hosted",
        "transcript_cwd":"/home/user/repo","collected_at":chrono::Utc::now().to_rfc3339(),
        "collection":"claude-teleport","completeness":"handoff-may-be-compacted","identity_basis":"collector_asserted"}});
    Ok((f, payload))
}
#[test]
fn malformed_provenance_never_creates_refs_or_pending_state() -> Result<()> {
    let (f, base) = fixture()?;
    for (field, value, error) in [
        (
            "remote_session_id",
            json!("session_../../escape"),
            "Invalid Claude remote",
        ),
        (
            "identity_basis",
            json!("provider_verified"),
            "collector asserted",
        ),
        (
            "repository",
            json!("../capture-test"),
            "Invalid hosted repository",
        ),
        ("source_branch", json!("bad..branch"), "Command failed"),
        (
            "source_commit",
            json!("HEAD"),
            "Invalid hosted source commit",
        ),
        ("transcript_cwd", json!("relative"), "must be absolute"),
        (
            "transcript_cwd",
            json!("/home/../repo"),
            "without parent traversal",
        ),
        (
            "collected_at",
            json!("tomorrow"),
            "Invalid hosted collection timestamp",
        ),
        (
            "collected_at",
            json!("2999-01-01T00:00:00Z"),
            "in the future",
        ),
    ] {
        let mut payload = base.clone();
        payload["provenance"][field] = value;
        let failure = support::invoke(
            &f.repo,
            &["capture-cloud", "claude-code", "-"],
            &f.env,
            Some(&payload),
        )
        .unwrap_err();
        ensure!(
            format!("{failure:#}").contains(error),
            "Unexpected failure: {failure:#}"
        );
        ensure!(
            f.g(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/trailvault/checkpoints/"
            ])?
            .is_empty()
        );
        ensure!(capture::sessions(&git::Repo::discover(&f.repo)?)?.is_empty());
    }
    Ok(())
}
#[test]
fn missing_malformed_oversized_and_non_utf8_handoffs_fail_explicitly() -> Result<()> {
    let (f, base) = fixture()?;
    let path = std::path::Path::new(base["transcript_path"].as_str().unwrap());
    for (content, message) in [
        (b"{broken}\n".as_slice(), "Invalid transcript line"),
        (b"\xff\n".as_slice(), "not UTF-8"),
        (b"".as_slice(), "transcript is empty"),
    ] {
        put(path, content)?;
        let failure = support::invoke(
            &f.repo,
            &["capture-cloud", "claude-code", "-"],
            &f.env,
            Some(&base),
        )
        .unwrap_err();
        ensure!(
            format!("{failure:#}").contains(message),
            "Unexpected failure: {failure:#}"
        );
    }
    fs::File::options()
        .write(true)
        .open(path)?
        .set_len(64 * 1024 * 1024 + 1)?;
    let failure = support::invoke(
        &f.repo,
        &["capture-cloud", "claude-code", "-"],
        &f.env,
        Some(&base),
    )
    .unwrap_err();
    ensure!(format!("{failure:#}").contains("Hosted transcript exceeds 64 MiB"));
    ensure!(
        f.g(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/trailvault/checkpoints/"
        ])?
        .is_empty()
    );
    Ok(())
}
#[test]
fn handoff_import_never_replaces_same_uuid_local_session() -> Result<()> {
    let (f, payload) = fixture()?;
    support::invoke(
        &f.repo,
        &["capture", "claude-code", "session-start", "-"],
        &f.env,
        Some(&json!({"sessionId":payload["native_session_id"]})),
    )?;
    let repo = git::Repo::discover(&f.repo)?;
    let state = serde_json::to_value(capture::sessions(&repo)?)?;
    support::invoke(
        &f.repo,
        &["capture-cloud", "claude-code", "-"],
        &f.env,
        Some(&payload),
    )?;
    ensure!(serde_json::to_value(capture::sessions(&repo)?)? == state);
    Ok(())
}

#[test]
fn checkpoint_metadata_uses_hosted_source_not_collectors_checkout() -> Result<()> {
    let (f, mut payload) = fixture()?;
    payload["participants"] = json!([{"github_login":"fixture-human"}]);
    f.g(&[
        "commit",
        "--allow-empty",
        "-m",
        "Unrelated collector checkout commit",
    ])?;
    ensure!(
        f.g(&["rev-parse", "HEAD"])? != payload["provenance"]["source_commit"].as_str().unwrap()
    );
    ensure!(f.g(&["branch", "--show-current"])? != "test/hosted");
    let checkpoint = support::invoke(
        &f.repo,
        &["capture-cloud", "claude-code", "-"],
        &f.env,
        Some(&payload),
    )?;
    let reference = capture::checkpoint_ref(checkpoint.trim())?;
    for path in ["metadata.json", "0/metadata.json"] {
        let metadata: Value =
            serde_json::from_str(&f.g(&["show", &format!("{reference}:{path}")])?)?;
        ensure!(metadata["branch"] == "test/hosted");
        if path == "0/metadata.json" {
            ensure!(metadata["participants"] == payload["participants"]);
            ensure!(metadata["participant_identity_basis"] == "capture_input");
        }
    }
    let evidence: Value =
        serde_json::from_str(&f.g(&["show", &format!("{reference}:0/evidence.json")])?)?;
    ensure!(evidence["source_head_sha"] == payload["provenance"]["source_commit"]);
    ensure!(evidence["branch"] == "test/hosted");
    Ok(())
}
