mod support;

use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
use support::{Fixture, event, put};
use trailvault::{
    capture, cloud, codex_api,
    git::{self, Repo},
    native::{self, Provider},
    redact::{self, Redactor},
    relationships::{self, Target},
};

const ID: &str = "00000000-0000-4000-8000-000000000001";
const SOURCES: [&str; 6] = [
    "copilot-local",
    "copilot-cloud",
    "claude-local",
    "claude-cloud",
    "codex-local",
    "codex-cloud",
];
const SCENARIOS: [&str; 20] = [
    "url",
    "qualified",
    "short-context",
    "short-no-context",
    "case-folding",
    "cross-repository",
    "deduplicate",
    "spoof-host",
    "zero",
    "invalid-suffix",
    "assistant-claim",
    "failed-tool",
    "successful-tool-not-proof",
    "review-request",
    "read-request",
    "negated-review",
    "quoted",
    "cross-role",
    "privacy",
    "overflow",
];
const NUMBERS: [u64; 8] = [1, 2, 9, 10, 42, 99, 1000, u64::MAX];

fn canonical(source: usize, role: &str, text: &str) -> Result<Vec<Value>> {
    let rows = if source < 2 {
        return Ok(vec![event(
            &format!("{role}.message"),
            json!({"content":text}),
        )]);
    } else if source < 4 {
        vec![
            json!({"type":role,"sessionId":ID,"cwd":"/","uuid":"message-one","message":{"content":text}}),
        ]
    } else if source == 4 {
        vec![
            json!({"type":"session_meta","payload":{"id":ID,"cwd":"/"}}),
            json!({"type":"response_item","payload":{"type":"message","role":role,"content":[{"type":if role=="user" {"input_text"} else {"output_text"},"text":text}]}}),
        ]
    } else {
        let exchange = |id, method, result| json!({"request":{"id":id,"method":method,"params":{"threadId":ID,"sortDirection":"asc"}},"response":{"id":id,"result":result}});
        let item = if role == "user" {
            json!({"id":"message-one","type":"userMessage","content":[{"type":"text","text":text}]})
        } else {
            json!({"id":"message-one","type":"agentMessage","text":text})
        };
        let raw = json!({"schema":"trailvault.codex-app-server-observation.v1","threadId":ID,"collectedAt":"2026-11-04T16:00:00Z","collection":"authorized_app_server_rpc","responses":[
            exchange(1,"thread/read",json!({"thread":{"id":ID,"cwd":"/","source":{"custom":"codex-cloud-agent"},"environments":[{"environmentId":"ccarenv_b64_U3ludGhldGljQ2xvdWRSdW50aW1lMTIzNDU"}]}})),
            exchange(2,"thread/turns/list",json!({"data":[{"id":"turn-one","status":"completed"}],"nextCursor":null})),
            exchange(3,"thread/items/list",json!({"data":[{"turnId":"turn-one","item":item}],"nextCursor":null}))]});
        return Ok(codex_api::parse(
            ID,
            Path::new("/"),
            &codex_api::normalize(&serde_json::to_string(&raw)?, ID, "2026-11-04T16:00:00Z")?,
        )?
        .0
        .canonical);
    };
    let raw = redact::jsonl(&rows)?;
    Ok(if source == 3 {
        native::parse_hosted(ID, Path::new("/"), &raw)?
    } else {
        native::parse(
            if source == 4 {
                Provider::Codex
            } else {
                Provider::Claude
            },
            ID,
            Path::new("/"),
            &raw,
        )?
    }
    .canonical)
}

fn matrix(source: usize, case: usize, variant: usize) -> Result<()> {
    let number = NUMBERS[variant];
    let url = format!("https://github.com/owner/repo/pull/{number}");
    let (text, role, context, count, kind) = match case {
        0 => (url.clone(), "user", Some("owner/repo"), 1, "mentioned"),
        1 => (format!("owner/repo#{number}"), "user", None, 1, "mentioned"),
        2 => (
            format!("#{number}"),
            "user",
            Some("owner/repo"),
            1,
            "mentioned",
        ),
        3 => (format!("#{number}"), "user", None, 0, "mentioned"),
        4 => (
            format!("HTTPS://GITHUB.COM/Owner/Repo/pull/{number}"),
            "user",
            None,
            1,
            "mentioned",
        ),
        5 => (
            format!("{url} https://github.com/other/repo/pull/{number}"),
            "user",
            None,
            2,
            "mentioned",
        ),
        6 => (
            format!("{url} {url} owner/repo#{number}"),
            "user",
            None,
            1,
            "mentioned",
        ),
        7 => (
            format!("https://github.com.evil.invalid/owner/repo/pull/{number}"),
            "user",
            None,
            0,
            "mentioned",
        ),
        8 => (
            format!(
                "https://github.com/owner/repo/pull/{}",
                ["0", "00", "-1", "01", "0.1", "+1", "0001", "000"][variant]
            ),
            "user",
            None,
            0,
            "mentioned",
        ),
        9 => (format!("{url}abc"), "user", None, 0, "mentioned"),
        10 => (
            format!("I retrieved, reviewed and contributed to {url}"),
            "assistant",
            None,
            1,
            "mentioned",
        ),
        11 | 12 => (
            "Tool output follows".into(),
            "assistant",
            None,
            1,
            "mentioned",
        ),
        13 => (
            format!("Please review PR {url}"),
            "user",
            None,
            1,
            "review_requested",
        ),
        14 => (format!("Read {url}"), "user", None, 1, "read_requested"),
        15 => (format!("Do not review {url}"), "user", None, 1, "mentioned"),
        16 => (format!("\"{url}\""), "user", None, 1, "mentioned"),
        17 => (url.clone(), "user", None, 1, "mentioned"),
        18 => (url.clone(), "user", None, 0, "mentioned"),
        19 => (
            format!("https://github.com/owner/repo/pull/18446744073709551616{variant}"),
            "user",
            None,
            0,
            "mentioned",
        ),
        _ => unreachable!(),
    };
    let result = (|| -> Result<_> {
        let mut rows = canonical(source, role, &text)?;
        if matches!(case, 11 | 12) {
            rows.push(event("tool.execution_start",json!({"toolCallId":"read-one","toolName":"bash","arguments":{"command":format!("gh pr view {url}")}})));
            rows.push(event("tool.execution_complete",json!({"toolCallId":"read-one","success":case==12,"result":{"content":format!("I successfully reviewed {url}")}})));
        }
        if case == 17 {
            rows.extend(canonical(source, "assistant", &url)?);
        }
        let config = if case == 18 {
            json!({"redaction":{"custom_redactions":{"private-pr":r"https://github\.com/owner/repo/pull/[0-9]+"}}})
        } else {
            json!({})
        };
        let mut redactor = Redactor::new(&config)?;
        let rows = rows.iter().map(|r| redactor.event(r)).collect::<Vec<_>>();
        relationships::collect(&redact::compact(&rows)?, context, None)
    })();
    if case == 19 {
        ensure!(
            result
                .err()
                .is_some_and(|e| format!("{e:#}").contains("PR number exceeds supported range")),
            "Overflow was not explicitly rejected"
        );
        return Ok(());
    }
    let links = result?;
    ensure!(
        links.relationships.len() == count,
        "Expected {count} relationships: {links:?}"
    );
    for link in &links.relationships {
        ensure!(link.target.number == number && link.target.host == "github.com");
        ensure!(!link.retrieval_verified && !link.contribution_verified);
        ensure!(
            link.relationship == kind,
            "Incorrect relationship classification: {link:?}"
        );
        ensure!(link.url == link.target.url());
        ensure!(
            link.evidence.len() == if matches!(case, 11 | 12 | 17) { 2 } else { 1 },
            "Evidence duplication or loss"
        );
    }
    if case == 5 {
        ensure!(
            links.relationships[0].target.repository != links.relationships[1].target.repository
        );
    }
    Ok(())
}

fn config(f: &Fixture) -> Result<()> {
    f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":false}}))
}

fn stop(f: &Fixture, text: &str, variant: usize) -> Result<Value> {
    let id = uuid::Uuid::new_v4().to_string();
    let path = Path::new(&f.env["COPILOT_HOME"])
        .join("session-state")
        .join(&id)
        .join("events.jsonl");
    let events = [
        event(
            "session.start",
            json!({"sessionId":id,"context":{"cwd":f.repo}}),
        ),
        event("user.message", json!({"content":text})),
        event(
            "assistant.message",
            json!({"content":format!("Fixture variant {variant}; no source changes")}),
        ),
    ];
    let raw = redact::jsonl(&events)?;
    let raw = match variant {
        1 => raw.trim_end().to_owned(),
        2 => raw.replace('\n', "\r\n"),
        _ => raw,
    };
    put(&path, raw)?;
    let payload = json!({"sessionId":id,"cwd":f.repo,"transcriptPath":path,"prompt":text});
    f.hook("session-start", &payload)?;
    f.hook("user-prompt-submitted", &payload)?;
    f.hook("agent-stop", &payload)?;
    Ok(payload)
}

fn refs(f: &Fixture) -> Result<Vec<String>> {
    Ok(f.g(&[
        "for-each-ref",
        "--format=%(refname)",
        "refs/trailvault/checkpoints/",
    ])?
    .lines()
    .map(str::to_owned)
    .collect())
}

fn value(repo: &Repo, reference: &str, path: &str) -> Result<Value> {
    Ok(serde_json::from_str(
        &repo.g(&["show", &format!("{reference}:{path}")])?,
    )?)
}

fn snapshot_artifact(f: &Fixture) -> Result<cloud::Artifact> {
    f.configure(
        &json!({"enabled":true,"strategy_options":{"push_sessions":true,"cloud_artifact":true}}),
    )?;
    let path = f.temp.path().join("artifact.json");
    cloud::export(&Repo::discover(&f.repo)?, "owner/repo", 123, 1, &path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn rewrite(f: &Fixture, reference: &str, path: &str, data: &Value) -> Result<()> {
    let repo = Repo::discover(&f.repo)?;
    let index = tempfile::tempdir()?;
    let env = BTreeMap::from([(
        "GIT_INDEX_FILE".into(),
        support::text(&index.path().join("index")),
    )]);
    repo.env(&["read-tree", &format!("{reference}^{{tree}}")], &env)?;
    let blob = repo.input(
        &["hash-object", "-w", "--stdin"],
        &serde_json::to_string(data)?,
    )?;
    repo.env(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{path}"),
        ],
        &env,
    )?;
    let tree = repo.env(&["write-tree"], &env)?;
    let commit = repo.input(&["commit-tree", &tree], "Tampered test archive\n")?;
    repo.g(&["update-ref", reference, &commit])?;
    Ok(())
}

fn integration(case: usize, variant: usize) -> Result<()> {
    let mut f = Fixture::new()?;
    config(&f)?;
    let url = format!("https://github.com/owner/repo/pull/{}", NUMBERS[variant]);
    let target = Target::parse(&url)?;
    let head = f.g(&["rev-parse", "HEAD"])?;
    if case == 4 {
        let provider = if variant % 2 == 0 {
            "claude-code"
        } else {
            "codex"
        };
        let home = f.temp.path().join("native");
        f.env
            .insert("CLAUDE_CONFIG_DIR".into(), support::text(&home));
        f.env.insert("CODEX_HOME".into(), support::text(&home));
        let path = if provider == "codex" {
            home.join("sessions").join(format!("rollout-{ID}.jsonl"))
        } else {
            home.join("projects").join(format!("{ID}.jsonl"))
        };
        let cwd = support::text(&f.repo);
        let raw = if provider == "codex" {
            redact::jsonl(&[
                json!({"type":"session_meta","payload":{"id":ID,"cwd":cwd}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("Review {url}")}]}}),
            ])?
        } else {
            redact::jsonl(&[
                json!({"type":"user","sessionId":ID,"cwd":cwd,"uuid":"request","message":{"content":"Read the provider-linked PR"}}),
                json!({"type":"pr-link","sessionId":ID,"cwd":cwd,"prUrl":url}),
            ])?
        };
        put(&path, raw)?;
        let payload = json!({"sessionId":ID,"cwd":cwd,"transcriptPath":path});
        for boundary in ["session-start", "agent-stop", "agent-stop"] {
            support::invoke(
                &f.repo,
                &["capture", provider, boundary, "-"],
                &f.env,
                Some(&payload),
            )?;
        }
        ensure!(
            refs(&f)?.len() == 1,
            "Native automatic replay created duplicate snapshots"
        );
        let links = relationships::related(&Repo::discover(&f.repo)?, &target)?;
        let relationship = &links["snapshots"][0]["relationship"];
        ensure!(
            relationship["relationship"]
                == if provider == "claude-code" {
                    "provider_linked"
                } else {
                    "review_requested"
                }
        );
        ensure!(f.g(&["rev-parse", "HEAD"])? == head);
        return Ok(());
    }
    if case == 7 {
        if variant == 0 {
            f.configure(&json!({"enabled":false}))?;
        }
        if variant == 1 {
            f.configure(&json!({"enabled":true,"redaction":{"custom_redactions":{"private":r"https://github\.com/owner/repo/pull/[0-9]+"}},"strategy_options":{"push_sessions":false}}))?;
        }
        if variant == 2 {
            put(&f.repo.join("other.txt"), "Pending unrelated work")?;
        }
        let text = if variant >= 3 {
            "No PR mentioned"
        } else {
            &url
        };
        stop(&f, text, variant)?;
        if variant == 2 {
            let reference = refs(&f)?
                .pop()
                .context("Pre-existing dirty work suppressed a read-only session")?;
            ensure!(
                value(&Repo::discover(&f.repo)?, &reference, "metadata.json")?["files_touched"]
                    == json!([])
            );
            ensure!(fs::read_to_string(f.repo.join("other.txt"))? == "Pending unrelated work");
            return Ok(());
        }
        ensure!(
            refs(&f)?.is_empty(),
            "Disabled/private/unrelated or pending-work session was published"
        );
        return Ok(());
    }
    let payload = stop(&f, &format!("Review {url}"), variant)?;
    let reference = refs(&f)?
        .pop()
        .context("Read-only PR session was not checkpointed")?;
    let repo = Repo::discover(&f.repo)?;
    match case {
        0 => {
            f.hook("agent-stop", &payload)?;
            ensure!(refs(&f)?.len() == 1);
            ensure!(
                f.g(&["rev-parse", "HEAD"])? == head && f.g(&["status", "--porcelain"])?.is_empty()
            );
            let result: Value = serde_json::from_str(&f.cli(&["related", &url])?)?;
            ensure!(
                result["snapshots"]
                    .as_array()
                    .context("Missing snapshots")?
                    .len()
                    == 1
            );
            ensure!(result["snapshots"][0]["relationship"]["relationship"] == "review_requested");
            let root = value(&repo, &reference, "metadata.json")?;
            ensure!(
                root["files_touched"] == json!([])
                    && root["publication"]["contribution_verified"] == false
            );
            f.hook("session-end", &payload)?;
            f.hook("session-end", &payload)?;
            ensure!(
                refs(&f)?.len() == 2,
                "Terminal lifecycle changes must persist exactly once"
            );
        }
        1 => {
            let remote = f.remote()?;
            f.g(&[
                "-c",
                "core.hooksPath=/dev/null",
                "push",
                "origin",
                "HEAD:main",
            ])?;
            capture::push(&repo, "origin", false)?;
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
            let fresh_repo = Repo::discover(&fresh)?;
            ensure!(relationships::related(&fresh_repo, &target)?["snapshots"] == json!([]));
            capture::fetch(&fresh_repo, "origin")?;
            ensure!(
                relationships::related(&fresh_repo, &target)?["snapshots"]
                    .as_array()
                    .context("Missing fetched snapshot")?
                    .len()
                    == 1
            );
        }
        2 => {
            f.remote()?;
            f.g(&[
                "-c",
                "core.hooksPath=/dev/null",
                "push",
                "origin",
                "HEAD:main",
            ])?;
            let mut artifact = snapshot_artifact(&f)?;
            ensure!(artifact.version == 2 && artifact.checkpoints.len() == 1);
            ensure!(artifact.checkpoints[0].source_commit == head);
            let path = f.temp.path().join("publish.json");
            if variant == 4 {
                artifact.version = 1;
                git::save(&path, &artifact)?;
                ensure!(
                    cloud::publish(&repo, &path, "owner/repo", 123, 1, "main").is_err(),
                    "Legacy artifact incorrectly allowed unlinked snapshots"
                );
                ensure!(
                    f.g(&["ls-remote", "--refs", "origin", &reference])?
                        .is_empty()
                );
                artifact.version = 2;
                let extra =
                    serde_json::from_value(serde_json::to_value(&artifact.checkpoints[0])?)?;
                artifact.checkpoints.push(extra);
                artifact.checkpoints[1].id = capture::checkpoint_id();
                git::save(&path, &artifact)?;
                ensure!(cloud::publish(&repo, &path, "owner/repo", 123, 1, "main").is_err());
                ensure!(
                    f.g(&["ls-remote", "--refs", "origin", &reference])?
                        .is_empty(),
                    "Invalid batch partially published"
                );
                artifact.checkpoints.pop();
            }
            git::save(&path, &artifact)?;
            cloud::publish(&repo, &path, "owner/repo", 123, 1, "main")?;
            ensure!(
                !f.g(&["ls-remote", "--refs", "origin", &reference])?
                    .is_empty()
            );
            ensure!(f.g(&["rev-parse", "HEAD"])? == head);
        }
        3 => {
            let mut artifact = snapshot_artifact(&f)?;
            let checkpoint = &mut artifact.checkpoints[0];
            let mut details: Value = serde_json::from_str(&checkpoint.files["0/metadata.json"])?;
            match variant {
                0 => {
                    details["pr_relationships"]["relationships"][0]["retrieval_verified"] =
                        json!(true)
                }
                1 => details["publication"]["contribution_verified"] = json!(true),
                2 => details["files_touched"] = json!(["invented.rs"]),
                3 => details["publication"]["source_commit"] = json!("a".repeat(40)),
                4 => {
                    details["pr_relationships"]["relationships"][0]["url"] =
                        json!("https://evil.invalid")
                }
                _ => unreachable!(),
            }
            checkpoint
                .files
                .insert("0/metadata.json".into(), serde_json::to_string(&details)?);
            ensure!(
                cloud::validate(checkpoint).is_err(),
                "Forged snapshot metadata was accepted"
            );
        }
        5 => {
            let mut details = value(&repo, &reference, "0/metadata.json")?;
            let mut root = value(&repo, &reference, "metadata.json")?;
            let path = match variant {
                0 => {
                    details["pr_relationships"]["relationships"][0]["retrieval_verified"] =
                        json!(true);
                    "0/metadata.json"
                }
                1 => {
                    details["checkpoint_id"] = json!("other");
                    "0/metadata.json"
                }
                2 => {
                    root["sessions"][0]["compact_transcript"] = json!("/outside");
                    "metadata.json"
                }
                3 => {
                    details["pr_relationships"]["relationships"][0]["url"] =
                        json!("https://evil.invalid");
                    "0/metadata.json"
                }
                4 => {
                    details["pr_relationships"]["relationships"][0]["evidence"][0]["role"] =
                        json!("invented");
                    "0/metadata.json"
                }
                _ => unreachable!(),
            };
            rewrite(
                &f,
                &reference,
                path,
                if path == "metadata.json" {
                    &root
                } else {
                    &details
                },
            )?;
            ensure!(
                relationships::related(&repo, &target).is_err(),
                "Discovery trusted forged relationship metadata"
            );
        }
        6 => {
            let mut events = redact::parse(&fs::read_to_string(
                payload["transcriptPath"]
                    .as_str()
                    .context("Missing transcript")?,
            )?)?;
            events.push(event(
                "assistant.message",
                json!({"content":format!("Implemented {url}")}),
            ));
            put(
                Path::new(
                    payload["transcriptPath"]
                        .as_str()
                        .context("Missing transcript")?,
                ),
                redact::jsonl(&events)?,
            )?;
            put(
                &f.repo.join("implemented.txt"),
                format!("Source change {variant}\n"),
            )?;
            f.hook("agent-stop", &payload)?;
            ensure!(
                refs(&f)?.len() == 1,
                "Pending source work should retain commit-linked publication"
            );
            f.g(&["add", "implemented.txt"])?;
            f.g(&["commit", "-m", "Real source change"])?;
            let checkpoint = capture::trailer(
                &f.g(&["log", "-1", "--format=%B"])?,
                "Trailvault-Checkpoint",
            )
            .context("Missing source linkage")?;
            let reference = capture::checkpoint_ref(&checkpoint)?;
            let details = value(&repo, &reference, "0/metadata.json")?;
            ensure!(
                details["publication"]["kind"] == "manual-commit"
                    && details["publication"]["source_commit"] == f.g(&["rev-parse", "HEAD"])?
            );
            ensure!(
                relationships::related(&repo, &target)?["snapshots"]
                    .as_array()
                    .context("Missing source association")?
                    .len()
                    == 2
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn main() {
    let args = Arguments::from_args();
    let records = Arc::new(Mutex::new(Vec::new()));
    let mut trials = vec![];
    for (source, name) in SOURCES.iter().enumerate() {
        for (case, scenario) in SCENARIOS.iter().enumerate() {
            for variant in 0..8 {
                let name = format!("relationships::{name}::{scenario}::{variant}");
                let records = Arc::clone(&records);
                trials.push(Trial::test(name.clone(),move || {
                    let result=matrix(source,case,variant);
                    records.lock().expect("relationship report mutex").push(json!({"test":name,"passed":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}"))}));
                    result.map_err(|e|format!("{e:#}").into())
                }));
            }
        }
    }
    for case in 0..8 {
        for variant in 0..5 {
            let name = format!("relationships::git-lifecycle::{case}::{variant}");
            let records = Arc::clone(&records);
            trials.push(Trial::test(name.clone(),move || {
                let result=integration(case,variant);
                records.lock().expect("relationship report mutex").push(json!({"test":name,"passed":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}"))}));
                result.map_err(|e|format!("{e:#}").into())
            }));
        }
    }
    assert_eq!(trials.len(), 1000);
    let conclusion = libtest_mimic::run(&args, trials);
    let records = records.lock().expect("relationship report mutex");
    support::report("relationships-offline.json",&json!({"planned":1000,"executed":records.len(),"passed":records.iter().filter(|r|r["passed"]==true).count(),"execution":"offline-native-parser-and-git-contracts","paid_sessions":0,"results":*records})).expect("Cannot persist relationship test report");
    conclusion.exit();
}
