mod support;

use anyhow::{Context, Result, ensure};
use serde_json::json;
use support::{Fixture, event, put};
use trailvault::{
    capture,
    git::{self, Repo},
};
use trailvault::{
    redact,
    relationships::{self, Target},
};

fn read_only(f: &Fixture) -> Result<serde_json::Value> {
    let id = uuid::Uuid::new_v4().to_string();
    let path = std::path::Path::new(&f.env["COPILOT_HOME"])
        .join("session-state")
        .join(&id)
        .join("events.jsonl");
    put(
        &path,
        redact::jsonl(&[
            event(
                "session.start",
                json!({"sessionId":id,"context":{"cwd":f.repo}}),
            ),
            event(
                "user.message",
                json!({"content":"https://github.com/owner/repo/pull/1"}),
            ),
        ])?,
    )?;
    let payload = json!({"sessionId":id,"transcriptPath":path,"cwd":f.repo});
    f.hook("session-start", &payload)?;
    f.hook("agent-stop", &payload)?;
    Ok(payload)
}

#[test]
fn target_urls_are_complete_and_unambiguous() -> Result<()> {
    for bad in [
        "https://github.com/o/r/pull/1 extra",
        "https://github.com/o/r/pull/1abc",
        "https://github.com/o/r/pull/1/../../other",
        "https://github.com/o/r/pull/0",
        "https://github.com.evil.invalid/o/r/pull/1",
        "https://github.com/o/r/pull/1\"",
    ] {
        ensure!(
            Target::parse(bad).is_err(),
            "Accepted malformed target {bad}"
        );
    }
    let target = Target::parse("https://github.com/Owner/Repo/pull/1/files#diff-one")?;
    ensure!(target.url() == "https://github.com/owner/repo/pull/1");
    Ok(())
}

#[test]
fn qualified_references_cannot_be_substrings_of_paths() -> Result<()> {
    for text in [
        "other/owner/repo#1",
        "https://github.com/owner/repo#1",
        "./owner/repo#1",
    ] {
        ensure!(relationships::mentions(text, None)?.is_empty(), "{text}");
    }
    ensure!(relationships::mentions("(owner/repo#1)", None)?.len() == 1);
    Ok(())
}

#[test]
fn short_review_requests_do_not_promote_other_repositories() -> Result<()> {
    let compact = redact::compact(&[
        json!({"type":"user.message","data":{"content":"Review #1 and do not review other/repo#1"}}),
    ])?;
    let links = relationships::collect(&compact, Some("owner/repo"), None)?;
    ensure!(links.relationships.len() == 2);
    for link in links.relationships {
        ensure!(
            link.relationship
                == if link.target.repository == "owner/repo" {
                    "review_requested"
                } else {
                    "mentioned"
                }
        );
    }
    Ok(())
}

#[test]
fn structured_tool_values_preserve_leading_shorthand_and_quoted_urls() -> Result<()> {
    let compact = redact::compact(&[
        json!({"type":"tool.execution_complete","data":{"toolCallId":"call-one","success":false,
        "result":{"nested":[{"text":"#1"},{"text":"\"https://github.com/other/repo/pull/1\""}]}}}),
    ])?;
    let links = relationships::collect(&compact, Some("owner/repo"), None)?;
    ensure!(links.relationships.len() == 2);
    ensure!(
        links
            .relationships
            .iter()
            .all(|l| l.relationship == "mentioned"
                && !l.retrieval_verified
                && !l.contribution_verified)
    );
    Ok(())
}

#[test]
fn malformed_contract_inputs_fail_explicitly() -> Result<()> {
    for compact in [
        "",
        r#"{"type":"message","role":"user","content":"owner/repo#1"}"#,
        r#"{"type":"header","version":99}"#,
    ] {
        ensure!(relationships::collect(compact, None, None).is_err());
    }
    let compact = redact::compact(&[])?;
    ensure!(relationships::collect(&compact, None, Some(&json!({"not":"an array"}))).is_err());
    Ok(())
}

#[test]
fn reference_limit_is_enforced_at_the_exact_boundary() -> Result<()> {
    let text = (1..=256)
        .map(|n| format!("owner/repo#{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    ensure!(relationships::mentions(&text, None)?.len() == 256);
    ensure!(relationships::mentions(&format!("{text} owner/repo#257"), None).is_err());
    Ok(())
}

#[test]
fn transcript_commands_remain_inert() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("never-execute");
    let compact = redact::compact(&[
        json!({"type":"user.message","data":{"content":format!("$(touch {}) https://github.com/owner/repo/pull/1",marker.display())}}),
    ])?;
    ensure!(
        relationships::collect(&compact, None, None)?
            .relationships
            .len()
            == 1
    );
    ensure!(!marker.exists());
    Ok(())
}

#[test]
fn repeated_boundary_retries_failed_publication_without_another_snapshot() -> Result<()> {
    let f = Fixture::new()?;
    f.configure(&json!({"enabled":true,"strategy_options":{"push_sessions":true}}))?;
    let remote = f.remote()?;
    f.g(&[
        "-c",
        "core.hooksPath=/dev/null",
        "push",
        "origin",
        "HEAD:main",
    ])?;
    f.g(&[
        "remote",
        "set-url",
        "origin",
        &support::text(&f.temp.path().join("unavailable.git")),
    ])?;
    let payload = read_only(&f)?;
    let repo = Repo::discover(&f.repo)?;
    let queue = repo.common.join("trailvault/push-queue.json");
    let pending: Vec<String> = git::read(&queue)?;
    ensure!(pending.len() == 1);
    f.g(&["remote", "set-url", "origin", &support::text(&remote)])?;
    f.hook("agent-stop", &payload)?;
    ensure!(git::read::<Vec<String>>(&queue)?.is_empty());
    ensure!(
        f.g(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/trailvault/checkpoints/"
        ])?
        .lines()
        .count()
            == 1
    );
    ensure!(
        !f.g(&["ls-remote", "--refs", "origin", &pending[0]])?
            .is_empty()
    );
    Ok(())
}

#[test]
fn hosted_snapshot_binds_to_frozen_source_not_collector_head() -> Result<()> {
    let f = Fixture::new()?;
    f.configure(&json!({"enabled":true,"strategy_options":{"push_sessions":false}}))?;
    f.g(&[
        "remote",
        "add",
        "origin",
        "https://github.com/owner/repo.git",
    ])?;
    let source = f.g(&["rev-parse", "HEAD"])?;
    put(
        &f.repo.join("collector-only.txt"),
        "Collector source change\n",
    )?;
    f.g(&["add", "collector-only.txt"])?;
    f.g(&[
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "-m",
        "Collector-only change",
    ])?;
    let collector = f.g(&["rev-parse", "HEAD"])?;
    let id = "00000000-0000-4000-8000-000000000001";
    let path = f.temp.path().join("selected-native.jsonl");
    put(
        &path,
        redact::jsonl(&[
            json!({"type":"user","uuid":"one","sessionId":id,"cwd":"/workspace",
        "message":{"content":"Review https://github.com/owner/repo/pull/1"}}),
        ])?,
    )?;
    let repo = Repo::discover(&f.repo)?;
    let checkpoint = capture::hosted_checkpoint(
        &repo,
        &json!({"native_session_id":id,"transcript_path":path,"provenance":{
        "remote_session_id":"session_RelationshipFixture","repository":"owner/repo","source_commit":source,
        "source_branch":"test/relationships","transcript_cwd":"/workspace","collected_at":"2026-11-04T16:00:00Z",
        "collection":"claude-teleport","completeness":"handoff-may-be-compacted","identity_basis":"collector_asserted"}}),
    )?;
    let reference = capture::checkpoint_ref(&checkpoint)?;
    let metadata: serde_json::Value =
        serde_json::from_str(&f.g(&["show", &format!("{reference}:0/metadata.json")])?)?;
    ensure!(metadata["publication"]["source_commit"] == source && source != collector);
    ensure!(metadata["files_touched"] == json!([]));
    ensure!(f.g(&["rev-parse", "HEAD"])? == collector);
    Ok(())
}

#[test]
fn legacy_compact_archives_are_discovered_without_rewriting_them() -> Result<()> {
    let f = Fixture::new()?;
    f.configure(&json!({"enabled":true,"strategy_options":{"push_sessions":false}}))?;
    read_only(&f)?;
    let repo = Repo::discover(&f.repo)?;
    let reference = f.g(&[
        "for-each-ref",
        "--format=%(refname)",
        "refs/trailvault/checkpoints/",
    ])?;
    let metadata_path = format!("{reference}:0/metadata.json");
    let mut metadata: serde_json::Value = serde_json::from_str(&f.g(&["show", &metadata_path])?)?;
    metadata
        .as_object_mut()
        .context("Expected session metadata object")?
        .remove("pr_relationships");
    let index = tempfile::tempdir()?;
    let env = std::collections::BTreeMap::from([(
        "GIT_INDEX_FILE".into(),
        support::text(&index.path().join("index")),
    )]);
    repo.env(&["read-tree", &format!("{reference}^{{tree}}")], &env)?;
    let blob = repo.input(
        &["hash-object", "-w", "--stdin"],
        &serde_json::to_string(&metadata)?,
    )?;
    repo.env(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},0/metadata.json"),
        ],
        &env,
    )?;
    let tree = repo.env(&["write-tree"], &env)?;
    let commit = repo.input(
        &["commit-tree", &tree],
        "Legacy-shaped relationship fixture\n",
    )?;
    repo.g(&["update-ref", &reference, &commit])?;
    let links = relationships::related(
        &repo,
        &Target::parse("https://github.com/owner/repo/pull/1")?,
    )?;
    ensure!(
        links["snapshots"]
            .as_array()
            .context("Missing snapshots")?
            .len()
            == 1
    );
    ensure!(f.g(&["rev-parse", &reference])? == commit);
    Ok(())
}

#[test]
fn reverted_source_changes_do_not_require_an_artificial_commit() -> Result<()> {
    let f = Fixture::new()?;
    f.configure(&json!({"enabled":true,"strategy_options":{"push_sessions":false}}))?;
    let head = f.g(&["rev-parse", "HEAD"])?;
    let payload = read_only(&f)?;
    let before = f.g(&[
        "for-each-ref",
        "--format=%(refname)",
        "refs/trailvault/checkpoints/",
    ])?;
    put(&f.repo.join("tracked.txt"), "Temporary edit\n")?;
    f.hook("agent-stop", &payload)?;
    ensure!(
        f.g(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/trailvault/checkpoints/"
        ])? == before
    );
    put(&f.repo.join("tracked.txt"), "original\n")?;
    let path = std::path::Path::new(
        payload["transcriptPath"]
            .as_str()
            .context("Missing transcript")?,
    );
    let mut rows = redact::parse(&std::fs::read_to_string(path)?)?;
    rows.push(event(
        "assistant.message",
        json!({"content":"Temporary changes reverted; review complete"}),
    ));
    put(path, redact::jsonl(&rows)?)?;
    f.hook("agent-stop", &payload)?;
    ensure!(
        f.g(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/trailvault/checkpoints/"
        ])?
        .lines()
        .count()
            == 2
    );
    ensure!(f.g(&["rev-parse", "HEAD"])? == head && f.g(&["status", "--porcelain"])?.is_empty());
    Ok(())
}
