mod support;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink};
use support::{Fixture, put};
use trailvault::{capture, git, redact, restore};

const ID: &str = "10000000-0000-4000-8000-000000000001";
const CHECKPOINT: &str = "01M670FBAQ4R23ZYAA58E8822X";

fn checkpoint(f: &Fixture, root: Option<Value>, metadata: Option<Value>) -> Result<String> {
    let full = redact::jsonl(&[
        support::event(
            "session.start",
            json!({"sessionId":ID,"startTime":"2026-10-30T18:16:43.852Z","version":1,"producer":"copilot-agent","copilotVersion":"1.0.105","context":{"cwd":"/fictional/original"}}),
        ),
        support::event(
            "user.message",
            json!({"content":"Remember fictional fern-lantern."}),
        ),
        support::event(
            "assistant.message",
            json!({"content":"Acknowledged.","messageId":"30000000-0000-4000-8000-000000000003"}),
        ),
    ])?;
    let files = [
        ("metadata.json", serde_json::to_string(&root.unwrap_or_else(||json!({"checkpoint_id":CHECKPOINT,"sessions":[{"metadata":"/0/metadata.json","transcript":"/0/full.jsonl","content_hash":"/0/content_hash.txt"}]})))?),
        ("0/metadata.json",serde_json::to_string(&metadata.unwrap_or_else(||json!({"checkpoint_id":CHECKPOINT,"session_id":ID,"agent":"Copilot CLI","redaction":{"replacements":0}})))?),
        ("0/full.jsonl",full.clone()),
        ("0/content_hash.txt",format!("sha256:{}\n",capture::hash(&full))),
    ];
    for (path, contents) in files {
        put(&f.repo.join(path), contents)?;
    }
    f.g(&["add", "."])?;
    f.g(&["commit", "-m", "Synthetic restoration checkpoint"])?;
    let sha = f.g(&["rev-parse", "HEAD"])?;
    f.g(&["update-ref", &capture::checkpoint_ref(CHECKPOINT)?, &sha])?;
    Ok(sha)
}
fn args<'a>(home: &'a str, work: &'a str) -> Vec<&'a str> {
    vec![
        "restore",
        CHECKPOINT,
        "--session",
        ID,
        "--copilot-home",
        home,
        "--working-directory",
        work,
    ]
}

#[test]
fn cli_loads_fetched_checkpoint_in_fresh_clone_without_changing_source_or_worktree() -> Result<()> {
    let f = Fixture::bare()?;
    let sha = checkpoint(&f, None, None)?;
    let source = f.g(&["show", &format!("{sha}:0/full.jsonl")])?;
    let remote = f.remote()?;
    f.g(&[
        "push",
        "origin",
        "main",
        &capture::checkpoint_ref(CHECKPOINT)?,
    ])?;
    let fresh = f.temp.path().join("fresh");
    support::g(
        f.temp.path(),
        &[
            "clone",
            "--branch",
            "main",
            &support::text(&remote),
            &support::text(&fresh),
        ],
    )?;
    let home = f.temp.path().canonicalize()?.join("restored home");
    let argv = args(
        home.to_str().context("home")?,
        fresh.to_str().context("work")?,
    );
    ensure!(
        support::invoke(&fresh, &argv, &f.env, None).is_err(),
        "Normal clone should lack custom refs"
    );
    support::g(
        &fresh,
        &[
            "fetch",
            "origin",
            &format!("{0}:{0}", capture::checkpoint_ref(CHECKPOINT)?),
        ],
    )?;
    let output = support::invoke(&fresh, &argv, &f.env, None)?;
    let receipt: Value = serde_json::from_str(&output)?;
    ensure!(receipt["checkpoint_commit"] == sha && receipt["dry_run"] == false);
    let id = receipt["restored_session_id"].as_str().context("id")?;
    ensure!(id != ID && uuid::Uuid::parse_str(id).is_ok());
    ensure!(
        home.join("session-state")
            .join(id)
            .join("events.jsonl")
            .is_file()
    );
    ensure!(support::g(&fresh, &["status", "--porcelain"])?.is_empty());
    ensure!(f.g(&["show", &format!("{sha}:0/full.jsonl")])? == source);
    Ok(())
}
#[test]
fn dry_run_reports_warnings_without_creating_a_home() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let home = f.temp.path().canonicalize()?.join("dry run home");
    let mut argv = args(
        home.to_str().context("home")?,
        f.repo.to_str().context("work")?,
    );
    argv.push("--dry-run");
    let report: Value = serde_json::from_str(&f.cli(&argv)?)?;
    ensure!(report["dry_run"] == true && !home.exists());
    ensure!(
        report["warnings"]
            .as_array()
            .context("warnings")?
            .iter()
            .any(|entry| entry["code"] == "runtime_state_not_restored")
    );
    Ok(())
}
#[test]
fn redacted_source_needs_explicit_lossy_consent() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(
        &f,
        None,
        Some(
            json!({"checkpoint_id":CHECKPOINT,"session_id":ID,"agent":"Copilot CLI","redaction":{"replacements":3}}),
        ),
    )?;
    let home = f.temp.path().canonicalize()?.join("redacted");
    let mut argv = args(
        home.to_str().context("home")?,
        f.repo.to_str().context("work")?,
    );
    ensure!(f.cli(&argv).is_err() && !home.exists());
    argv.push("--allow-lossy");
    let report: Value = serde_json::from_str(&f.cli(&argv)?)?;
    ensure!(
        report["warnings"]
            .as_array()
            .context("warnings")?
            .iter()
            .any(|entry| entry["code"] == "redacted_content")
    );
    Ok(())
}
#[test]
fn rejects_malicious_metadata_paths_without_creating_destination() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(
        &f,
        Some(
            json!({"checkpoint_id":CHECKPOINT,"sessions":[{"metadata":"/../secrets","transcript":"/0/full.jsonl","content_hash":"/0/content_hash.txt"}]}),
        ),
        None,
    )?;
    let home = f.temp.path().canonicalize()?.join("invalid");
    let error = f
        .cli(&args(
            home.to_str().context("home")?,
            f.repo.to_str().context("work")?,
        ))
        .err()
        .context("expected failure")?;
    ensure!(format!("{error:#}").contains("artifact path") && !home.exists());
    Ok(())
}
#[test]
fn missing_source_session_is_not_silently_substituted() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let home = f.temp.path().canonicalize()?.join("missing");
    let mut argv = args(
        home.to_str().context("home")?,
        f.repo.to_str().context("work")?,
    );
    argv[3] = "20000000-0000-4000-8000-000000000002";
    ensure!(f.cli(&argv).is_err() && !home.exists());
    Ok(())
}
#[test]
fn cli_rejects_missing_repeated_unknown_and_relative_destination_options() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let home = f.temp.path().canonicalize()?.join("invalid options");
    let normal = args(
        home.to_str().context("home")?,
        f.repo.to_str().context("work")?,
    );
    for extra in [
        vec!["--session", ID],
        vec!["--unknown"],
        vec!["--working-directory"],
        vec!["--dry-run", "--dry-run"],
    ] {
        let mut argv = normal.clone();
        argv.extend(extra);
        ensure!(f.cli(&argv).is_err() && !home.exists());
    }
    ensure!(f.cli(&["restore", CHECKPOINT]).is_err());
    ensure!(
        f.cli(&args("relative-home", f.repo.to_str().context("work")?))
            .is_err()
    );
    Ok(())
}
#[test]
fn cli_preserves_existing_home_files_and_separate_forks() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let home = f.temp.path().canonicalize()?.join("home");
    put(&home.join("config.json"), b"{\"unrelated\":true}\n")?;
    let argv = args(
        home.to_str().context("home")?,
        f.repo.to_str().context("work")?,
    );
    let a: Value = serde_json::from_str(&f.cli(&argv)?)?;
    let b: Value = serde_json::from_str(&f.cli(&argv)?)?;
    ensure!(a["restored_session_id"] != b["restored_session_id"]);
    ensure!(fs::read(home.join("config.json"))? == b"{\"unrelated\":true}\n");
    Ok(())
}
#[test]
fn cli_refuses_symlinked_session_state() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let home = f.temp.path().canonicalize()?.join("home");
    let outside = f.temp.path().join("outside");
    fs::create_dir(&home)?;
    fs::create_dir(&outside)?;
    symlink(&outside, home.join("session-state"))?;
    ensure!(
        f.cli(&args(
            home.to_str().context("home")?,
            f.repo.to_str().context("work")?
        ))
        .is_err()
    );
    ensure!(fs::read_dir(outside)?.next().is_none());
    Ok(())
}
#[test]
fn cli_help_explains_experimental_fork_before_git_discovery() -> Result<()> {
    let f = Fixture::bare()?;
    let output = support::invoke(f.temp.path(), &["restore", "--help"], &f.env, None)?;
    ensure!(output.contains("NEW local session") && output.contains("Experimental"));
    Ok(())
}

#[test]
fn native_loader_discoveries_are_enforced_before_writing() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let repo = git::Repo::discover(&f.repo)?;
    let options = restore::Options {
        working_directory: f.repo.clone(),
        allow_lossy: false,
    };
    for (field, expected) in [
        ("id", "UUID"),
        ("startTime", "startTime"),
        ("messageId", "messageId"),
    ] {
        let mut snapshot = restore::load(&repo, CHECKPOINT, ID)?;
        let mut events = redact::parse(&snapshot.full)?;
        match field {
            "id" => events[0]["id"] = json!("not-a-uuid"),
            "startTime" => {
                events[0]["data"]
                    .as_object_mut()
                    .context("header")?
                    .remove(field);
            }
            _ => {
                events[2]["data"]
                    .as_object_mut()
                    .context("assistant")?
                    .remove(field);
            }
        }
        snapshot.full = redact::jsonl(&events)?;
        snapshot.content_hash = format!("sha256:{}", capture::hash(&snapshot.full));
        let error = restore::prepare(&snapshot, &options)
            .err()
            .context("Expected native schema rejection")?;
        ensure!(format!("{error:#}").contains(expected), "{error:#}");
    }
    Ok(())
}

#[test]
fn workspace_uses_copilot_compatible_millisecond_utc_dates() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let snapshot = restore::load(&git::Repo::discover(&f.repo)?, CHECKPOINT, ID)?;
    let prepared = restore::prepare(
        &snapshot,
        &restore::Options {
            working_directory: f.repo.clone(),
            allow_lossy: false,
        },
    )?;
    let pattern = regex::Regex::new(
        r#"^(?:created_at|updated_at): "\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z"$"#,
    )?;
    let dates: Vec<_> = prepared
        .workspace
        .lines()
        .filter(|line| line.starts_with("created_at:") || line.starts_with("updated_at:"))
        .collect();
    ensure!(dates.len() == 2 && dates.iter().all(|line| pattern.is_match(line)));
    Ok(())
}

#[test]
fn unknown_versions_and_events_are_not_silently_trusted() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let mut snapshot = restore::load(&git::Repo::discover(&f.repo)?, CHECKPOINT, ID)?;
    let mut events = redact::parse(&snapshot.full)?;
    events[0]["data"]["copilotVersion"] = json!("future-version");
    snapshot.full = redact::jsonl(&events)?;
    snapshot.content_hash = format!("sha256:{}", capture::hash(&snapshot.full));
    let mut options = restore::Options {
        working_directory: f.repo.clone(),
        allow_lossy: false,
    };
    ensure!(restore::prepare(&snapshot, &options).is_err());
    options.allow_lossy = true;
    let prepared = restore::prepare(&snapshot, &options)?;
    ensure!(
        prepared.receipt["warnings"]
            .as_array()
            .context("warnings")?
            .iter()
            .any(|entry| entry["code"] == "untested_copilot_version")
    );
    Ok(())
}

#[test]
fn native_fork_clears_remote_control_and_in_use_flags() -> Result<()> {
    let f = Fixture::bare()?;
    checkpoint(&f, None, None)?;
    let mut snapshot = restore::load(&git::Repo::discover(&f.repo)?, CHECKPOINT, ID)?;
    let mut events = redact::parse(&snapshot.full)?;
    events[0]["data"]["alreadyInUse"] = json!(true);
    events[0]["data"]["remoteSteerable"] = json!(true);
    snapshot.full = redact::jsonl(&events)?;
    snapshot.content_hash = format!("sha256:{}", capture::hash(&snapshot.full));
    let prepared = restore::prepare(
        &snapshot,
        &restore::Options {
            working_directory: f.repo.clone(),
            allow_lossy: false,
        },
    )?;
    let restored = redact::parse(&prepared.events)?;
    ensure!(
        restored[0]["data"]["alreadyInUse"] == false
            && restored[0]["data"]["remoteSteerable"] == false
    );
    Ok(())
}
