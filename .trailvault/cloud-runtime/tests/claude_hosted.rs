mod support;

use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use support::{Fixture, put};
use trailvault::{capture, git, redact};

fn codex() -> bool {
    env!("CARGO_CRATE_NAME") == "codex_hosted"
}
fn label() -> &'static str {
    if codex() {
        "Codex Cloud"
    } else {
        "Claude Code Cloud"
    }
}
fn provider() -> &'static str {
    if codex() { "codex" } else { "claude-code" }
}
fn description(index: usize) -> &'static str {
    if codex() {
        match index {
            10 => return "provider-reported status is not remote stop",
            4 => return "remote and native API thread identities agree",
            5 => return "structured API collection disclosed",
            6 => return "available persisted history limits disclosed",
            _ => {}
        }
    }
    CHECKS[index]
}

fn api_trace(id: &str, profile: usize, timestamp: &str) -> Value {
    let turn = "cloud-turn-1";
    let mut rows = vec![
        json!({"turnId":turn,"item":{"id":"user-1","type":"userMessage","content":[{"type":"text","text":format!("{} https://github.com/madsbolaris/trailvault-capture-test/pull/20",text(profile))}]}}),
        json!({"turnId":turn,"item":{"id":"assistant-1","type":"agentMessage","text":text(profile)}}),
        json!({"turnId":turn,"item":{"id":"cloud-call-1","type":"commandExecution","command":"git diff --check","cwd":"/home/user/repo",
            "status":if profile==18 {"inProgress"} else if profile==12 {"declined"} else {"completed"},
            "exitCode":if profile==11 {json!(1)} else {json!(0)},"aggregatedOutput":text(profile)}}),
        json!({"turnId":turn,"item":{"id":"future-1","type":"future.cloud","value":{"text":text(profile)}}}),
    ];
    if profile == 13 {
        rows.push(json!({"turnId":turn,"item":{"id":"cloud-call-2","type":"mcpToolCall","server":"github","tool":"fetch_pr",
            "arguments":{"text":text(profile)},"status":"completed","result":{"content":text(profile)},"error":null}}));
    }
    let mut turns =
        vec![json!({"id":turn,"status":"completed","items":[],"itemsView":"notLoaded"})];
    if profile == 16 {
        turns.push(
            json!({"id":"cloud-turn-2","status":"completed","items":[],"itemsView":"notLoaded"}),
        );
        rows.push(json!({"turnId":"cloud-turn-2","item":{"id":"user-resumed","type":"userMessage","content":[{"type":"text","text":"Resumed cloud turn"}]}}));
    }
    for (index, row) in rows.iter_mut().enumerate() {
        row["startedAtMs"] = json!(1000 + index);
        row["completedAtMs"] = json!(2000 + index);
    }
    let exchange = |index: usize, method: &str, params: Value, result: Value| json!({"request":{"id":index,"method":method,"params":params},"response":{"id":index,"result":result}});
    let mut responses = vec![
        exchange(
            0,
            "thread/read",
            json!({"threadId":id,"includeTurns":false}),
            json!({"thread":{"id":id,"cwd":"/home/user/repo",
            "source":{"custom":"codex-cloud-agent"},"status":{"type":"idle"},"environments":[{"environmentId":"ccarenv_b64_U3ludGhldGljQ2xvdWRSdW50aW1lMTIzNDU","environmentConfigId":"00000000-0000-4000-8000-000000000001~cecfg_00000000000000000000000000000001"}]}}),
        ),
        exchange(
            1,
            "thread/turns/list",
            json!({"threadId":id,"sortDirection":"asc"}),
            json!({"data":turns,"nextCursor":null}),
        ),
    ];
    if profile == 16 {
        responses.push(exchange(
            3,
            "thread/items/list",
            json!({"threadId":id,"cursor":"opaque-next-page","sortDirection":"asc"}),
            json!({"data":rows.split_off(2),"nextCursor":null}),
        ));
        responses.push(exchange(
            2,
            "thread/items/list",
            json!({"threadId":id,"sortDirection":"asc"}),
            json!({"data":rows,"nextCursor":"opaque-next-page"}),
        ));
    } else {
        responses.push(exchange(
            2,
            "thread/items/list",
            json!({"threadId":id,"sortDirection":"asc"}),
            json!({"data":rows,"nextCursor":null}),
        ));
    }
    json!({"schema":"trailvault.codex-app-server-observation.v1","threadId":id,"collectedAt":timestamp,
        "collection":"passive_provider_client_rpc","responses":responses})
}

const CHECKS: &[&str] = &[
    "native identity retained",
    "cloud provider header",
    "remote identity retained",
    "published source snapshot retained",
    "remote and native identities remain distinct",
    "handoff collection disclosed",
    "compaction gap disclosed",
    "identity mapping not falsely authenticated",
    "remote state remains unknown",
    "no live lease",
    "client exit is not remote stop",
    "task completion remains unverified",
    "background scope unverified",
    "pending calls explicitly historical",
    "native user messages retained",
    "native assistant messages retained",
    "native tool IDs retained",
    "matched tool results retained",
    "failed tools not fabricated as success",
    "unknown native records retained",
    "PR URL evidence retained",
    "PR relationships conservative",
    "no verified retrieval inferred",
    "no verified contribution inferred",
    "local dirty files not attributed to cloud",
    "no cloud source changes inferred",
    "native transcript digest exact",
    "metadata indexes evidence",
    "checkpoint is session-only",
    "no source commit created",
    "no index mutation",
    "original transcript unchanged",
    "input permissions unchanged",
    "redaction applies to all published files",
    "prompt follows publication redaction",
    "fresh clone needs explicit fetch",
    "every fetched file byte identical",
    "checkpoint object identical",
    "queue drained",
    "repeat import is immutable",
    "malformed payload rejected before mutation",
    "invalid remote identity rejected",
    "wrong repository rejected",
    "missing source commit rejected",
    "unverified collection rejected",
    "false completeness rejected",
    "wrong native identity rejected",
    "wrong remote worktree rejected",
    "disabled capture rejected",
    "failed upload retains retry queue",
];
const PROFILES: &[&str] = &[
    "plain",
    "spaces",
    "unicode",
    "multiline",
    "quotes",
    "shell-literal",
    "html",
    "large",
    "credential",
    "custom",
    "pii",
    "failed-tool",
    "denied-tool",
    "parallel-tools",
    "unknown-event",
    "usage",
    "resumed-turn",
    "crlf",
    "pending-tool",
    "blank-lines",
];
fn text(profile: usize) -> String {
    match profile {
        1 => "cloud text with spaces".into(),
        2 => "caf\u{e9} \u{65e5}\u{672c}".into(),
        3 => "cloud line one\ncloud line two".into(),
        4 => "\"cloud quotes\" \\slash".into(),
        5 => "$(touch NEVER_EXECUTE_CLOUD); inert".into(),
        6 => "<script>cloud_inert()</script>".into(),
        7 => "synthetic cloud content ".repeat(128),
        8 => "DB_PASSWORD=synthetic-hosted-fixture-78296".into(),
        9 => "PRIVATE-CLOUD-123456".into(),
        10 => "cloud.fixture@example.invalid".into(),
        _ => format!("Synthetic hosted profile {profile}"),
    }
}
fn blob(repo: &Path, reference: &str, path: &str) -> Result<String> {
    let output = git::command(
        repo,
        Path::new("git"),
        &[
            "-c",
            "safe.bareRepository=all",
            "show",
            &format!("{reference}:{path}"),
        ],
        &BTreeMap::new(),
        None,
        Duration::from_secs(30),
    )?;
    ensure!(
        output.success,
        "Cannot read hosted checkpoint: {}",
        output.stderr
    );
    Ok(output.stdout)
}
fn files(repo: &Path, reference: &str) -> Result<BTreeMap<String, String>> {
    support::g(repo, &["ls-tree", "-r", "--name-only", reference])?
        .lines()
        .map(|path| Ok((path.into(), blob(repo, reference, path)?)))
        .collect()
}
fn invoke(f: &Fixture, payload: &Value) -> Result<String> {
    support::invoke(
        &f.repo,
        &["capture-cloud", provider(), "-"],
        &f.env,
        Some(payload),
    )
}
struct Case {
    f: Fixture,
    profile: usize,
    id: String,
    path: PathBuf,
    raw: String,
    payload: Value,
    reference: String,
    previous: String,
    previous_files: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
    fetched: BTreeMap<String, String>,
    fresh: PathBuf,
    remote: PathBuf,
    head: String,
    tree: String,
}
fn scenario(profile: usize) -> Result<Case> {
    let f = Fixture::new()?;
    let remote = f.remote()?;
    f.g(&["remote", "rename", "origin", "archive"])?;
    f.g(&[
        "remote",
        "add",
        "origin",
        "https://github.com/madsbolaris/trailvault-capture-test.git",
    ])?;
    let mut settings = json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":false}});
    if profile == 9 {
        settings["redaction"] = json!({"custom_redactions":{"fixture":"PRIVATE-CLOUD-[0-9]+"}});
    }
    if profile == 10 {
        settings["redaction"] = json!({"pii":{"enabled":true}});
    }
    f.configure(&settings)?;
    let id = uuid::Uuid::new_v4().to_string();
    let path = f.temp.path().join("downloaded-cloud.jsonl");
    let head = f.g(&["rev-parse", "HEAD"])?;
    let tree = f.g(&["write-tree"])?;
    let mut events = vec![
        json!({"type":"user","sessionId":id,"cwd":"/home/user/repo","message":{"content":format!("{} https://github.com/madsbolaris/trailvault-capture-test/pull/20",text(profile))}}),
        json!({"type":"assistant","sessionId":id,"cwd":"/home/user/repo","message":{"role":"assistant","model":"cloud-fixture","usage":{"input_tokens":17},"content":[
            {"type":"text","text":text(profile)},{"type":"tool_use","id":"cloud-call-1","name":"Bash","input":{"command":"git diff --check"}}]}}),
    ];
    if profile == 13 {
        events.push(json!({"type":"assistant","sessionId":id,"cwd":"/home/user/repo","message":{"content":[{"type":"tool_use","id":"cloud-call-2","name":"Read","input":{"file_path":"README.md"}}]}}));
    }
    if profile != 18 {
        events.push(json!({"type":"user","sessionId":id,"cwd":"/home/user/repo","message":{"content":[{"type":"tool_result","tool_use_id":"cloud-call-1","is_error":([11,12].contains(&profile)),"content":text(profile)}]}}));
    }
    events.push(json!({"type":"future.cloud","sessionId":id,"value":{"text":text(profile)}}));
    let mut raw = redact::jsonl(&events)?;
    if profile == 17 {
        raw = raw.replace('\n', "\r\n");
    }
    if profile == 19 {
        raw = format!("\n{}", raw.replace('\n', "\n\n"));
    }
    let timestamp = chrono::Utc::now().to_rfc3339();
    if codex() {
        raw = serde_json::to_string(&api_trace(&id, profile, &timestamp))? + "\n";
        if profile == 17 {
            raw = raw.replace('\n', "\r\n");
        }
        if profile == 19 {
            raw = format!("\n\n{raw}\n\n");
        }
    }
    put(&path, &raw)?;
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    put(
        &f.repo.join("local-only.txt"),
        "independent local change, not the cloud agent\n",
    )?;
    let payload = json!({"native_session_id":id,"transcript_path":path,"provenance":{
        "remote_session_id":if codex() { id.clone() } else { format!("session_CloudFixture{profile:08}") },"repository":"madsbolaris/trailvault-capture-test",
        "source_commit":head,"source_branch":"test/hosted-fixture","transcript_cwd":"/home/user/repo",
        "collected_at":timestamp,"collection":if codex() {"codex-app-server"} else {"claude-teleport"},
        "completeness":if codex() {"available-persisted-history"} else {"handoff-may-be-compacted"},"identity_basis":"collector_asserted"}});
    let previous = capture::checkpoint_ref(&invoke(&f, &payload)?)?;
    let previous_files = files(&f.repo, &previous)?;
    let reference = capture::checkpoint_ref(&invoke(&f, &payload)?)?;
    let captured = files(&f.repo, &reference)?;
    f.cli(&["push", "archive"])?;
    f.g(&["push", "archive", "main"])?;
    let fresh = f.temp.path().join("fresh");
    support::g(
        f.temp.path(),
        &[
            "clone",
            "--quiet",
            &support::text(&remote),
            &support::text(&fresh),
        ],
    )?;
    ensure!(
        support::g(
            &fresh,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/trailvault/checkpoints/"
            ]
        )?
        .is_empty()
    );
    support::invoke(&fresh, &["fetch", "origin"], &f.env, None)?;
    let fetched = files(&fresh, &reference)?;
    Ok(Case {
        f,
        profile,
        id,
        path,
        raw,
        payload,
        reference,
        previous,
        previous_files,
        files: captured,
        fetched,
        fresh,
        remote,
        head,
        tree,
    })
}
fn invalid(c: &Case, change: impl FnOnce(&mut Value), expected: &str) -> Result<bool> {
    let repo = git::Repo::discover(&c.f.repo)?;
    let before = c.f.g(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        "refs/trailvault/checkpoints/",
    ])?;
    let queue = fs::read(repo.common.join("trailvault/push-queue.json"))?;
    let mut payload = c.payload.clone();
    change(&mut payload);
    let error = invoke(&c.f, &payload).expect_err("Invalid hosted payload accepted");
    ensure!(
        format!("{error:#}").contains(expected),
        "Unexpected rejection: {error:#}"
    );
    ensure!(
        before
            == c.f.g(&[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/trailvault/checkpoints/"
            ])?
    );
    ensure!(queue == fs::read(repo.common.join("trailvault/push-queue.json"))?);
    Ok(true)
}
fn check(index: usize, c: &Case) -> Result<()> {
    let e: Value = serde_json::from_str(&c.files["0/evidence.json"])?;
    let m: Value = serde_json::from_str(&c.files["0/metadata.json"])?;
    let root: Value = serde_json::from_str(&c.files["metadata.json"])?;
    let compact = redact::parse(&c.files["0/transcript.jsonl"])?;
    let observations = e["observations"].as_array().context("No observations")?;
    let refs = e["pr_references"].as_array().context("No PR references")?;
    let valid = match index {
        0 => e["session_id"] == c.id && m["session_id"] == c.id,
        1 => compact[0]["agent"] == label() && m["agent"] == label(),
        2 => e["hosted"]["remote_session_id"] == c.payload["provenance"]["remote_session_id"],
        3 => e["source_head_sha"] == c.head && e["branch"] == "test/hosted-fixture",
        4 => (e["hosted"]["remote_session_id"] == e["session_id"]) == codex(),
        5 => {
            e["hosted"]["collection"]
                == if codex() {
                    "codex-app-server"
                } else {
                    "claude-teleport"
                }
        }
        6 => {
            e["hosted"]["completeness"]
                == if codex() {
                    "available-persisted-history"
                } else {
                    "handoff-may-be-compacted"
                }
        }
        7 => e["hosted"]["identity_basis"] == "collector_asserted",
        8 => e["lifecycle"]["state"] == "unknown",
        9 => {
            e["lifecycle"]["live_lease"] == false
                && e["lifecycle"]["presence"] == "historical_snapshot"
        }
        10 => {
            e["lifecycle"]["boundary"] == "cloud-import"
                && e["lifecycle"]["basis"]
                    .as_str()
                    .unwrap_or("")
                    .contains(if codex() {
                        "not a live lease or proof of remote termination"
                    } else {
                        "not remote termination"
                    })
        }
        11 => e["lifecycle"]["task_completion"] == "unverified",
        12 => e["lifecycle"]["background_jobs"] == "unverified",
        13 => {
            e["lifecycle"]["pending_calls_are_historical"] == true
                && e["lifecycle"]["pending_tool_calls"]
                    .as_array()
                    .context("No pending calls")?
                    .contains(&json!("cloud-call-1"))
                    == (c.profile == 18)
        }
        14 if codex() => {
            compact
                .iter()
                .any(|v| v["type"] == "message" && v["role"] == "user")
                && e["api_history"]["pagination"] == "available_history_fully_paged"
                && e["api_history"]["turn_count"] == if c.profile == 16 { 2 } else { 1 }
        }
        14 => redact::parse(&c.files["0/full.jsonl"])?
            .iter()
            .any(|v| v["type"] == "user" && v["message"]["content"].is_string()),
        15 => compact
            .iter()
            .any(|v| v["type"] == "message" && v["role"] == "assistant"),
        16 => observations
            .iter()
            .any(|v| v["kind"] == "tool.started" && v["call_id"] == "cloud-call-1"),
        17 => {
            observations
                .iter()
                .any(|v| v["kind"] == "tool.finished" && v["call_id"] == "cloud-call-1")
                == (c.profile != 18)
        }
        18 => {
            observations
                .iter()
                .any(|v| v["kind"] == "tool.finished" && v["status"] == "failed")
                == [11, 12].contains(&c.profile)
        }
        19 if codex() => c.files["0/full.jsonl"].contains("\"type\":\"future.cloud\""),
        19 => redact::parse(&c.files["0/full.jsonl"])?
            .iter()
            .any(|v| v["type"] == "future.cloud"),
        20 => refs
            .iter()
            .any(|v| v["url"] == "https://github.com/madsbolaris/trailvault-capture-test/pull/20"),
        21 => refs.iter().all(|v| v["relationship"] == "mentioned"),
        22 => refs.iter().all(|v| v["retrieval_verified"] == false),
        23 => refs.iter().all(|v| v["contribution_verified"] == false),
        24 => e["dirty_files"] == json!([]),
        25 => m["files_touched"] == json!([]),
        26 => {
            e["native_sha256"] == capture::hash(&c.files["0/full.jsonl"])
                && c.files["0/content_hash.txt"]
                    == format!("sha256:{}\n", capture::hash(&c.files["0/full.jsonl"]))
        }
        27 => root["sessions"][0]["evidence"] == "/0/evidence.json",
        28 => {
            m["strategy"] == "session-snapshot"
                && c.f.g(&["rev-list", "--count", &c.reference])? == "1"
        }
        29 => c.f.g(&["rev-parse", "HEAD"])? == c.head,
        30 => c.f.g(&["write-tree"])? == c.tree,
        31 => fs::read_to_string(&c.path)? == c.raw,
        32 => {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&c.path)?.permissions().mode() & 0o777 == 0o600
        }
        33 => {
            let needle = serde_json::to_string(&text(c.profile))?;
            if [8, 9, 10].contains(&c.profile) {
                c.files.values().all(|s| !s.contains(&text(c.profile)))
                    && c.previous_files
                        .values()
                        .all(|s| !s.contains(&text(c.profile)))
            } else {
                c.files["0/full.jsonl"].contains(&needle[1..needle.len() - 1])
            }
        }
        34 => {
            c.files["0/prompt.txt"].contains(&text(c.profile)) == ![8, 9, 10].contains(&c.profile)
        }
        35 => c.fetched.len() == 7,
        36 => c.files == c.fetched,
        37 => {
            c.f.g(&["rev-parse", &c.reference])?
                == support::g(&c.fresh, &["rev-parse", &c.reference])?
        }
        38 => {
            git::read::<Value>(
                &git::Repo::discover(&c.f.repo)?
                    .common
                    .join("trailvault/push-queue.json"),
            )? == json!([])
        }
        39 => c.reference != c.previous && c.previous_files == files(&c.f.repo, &c.previous)?,
        40 => invalid(c, |p| p["extra"] = json!(true), "unknown field")?,
        41 => invalid(
            c,
            |p| p["provenance"]["remote_session_id"] = json!("https://untrusted.invalid/session"),
            if codex() {
                "Invalid Codex remote"
            } else {
                "Invalid Claude remote"
            },
        )?,
        42 => invalid(
            c,
            |p| p["provenance"]["repository"] = json!("other/repository"),
            "does not match origin",
        )?,
        43 => invalid(
            c,
            |p| {
                p["provenance"]["source_commit"] = json!("0000000000000000000000000000000000000000")
            },
            "Command failed",
        )?,
        44 => invalid(
            c,
            |p| p["provenance"]["collection"] = json!("guessed-api"),
            "Unsupported hosted collection",
        )?,
        45 => invalid(
            c,
            |p| p["provenance"]["completeness"] = json!("complete"),
            if codex() {
                "available-history limits"
            } else {
                "incomplete-history risk"
            },
        )?,
        46 => invalid(
            c,
            |p| p["native_session_id"] = json!(uuid::Uuid::new_v4().to_string()),
            if codex() {
                "thread identity mismatch"
            } else {
                "different session identity"
            },
        )?,
        47 => invalid(
            c,
            |p| p["provenance"]["transcript_cwd"] = json!("/other/worktree"),
            "different worktree",
        )?,
        48 => {
            let config = capture::settings(&git::Repo::discover(&c.f.repo)?)?;
            let mut disabled = config.clone();
            disabled["enabled"] = json!(false);
            c.f.configure(&disabled)?;
            let result = invalid(c, |_| {}, "Hosted capture is disabled");
            c.f.configure(&config)?;
            result?
        }
        49 => {
            let id = invoke(&c.f, &c.payload)?;
            let bad = c.f.temp.path().join("missing-remote");
            let failed = c.f.cli(&["push", &support::text(&bad)]).is_err();
            let repo = git::Repo::discover(&c.f.repo)?;
            let queued = git::read::<Value>(&repo.common.join("trailvault/push-queue.json"))?
                == json!([capture::checkpoint_ref(&id)?]);
            c.f.cli(&["push", "archive"])?;
            failed
                && queued
                && blob(&c.remote, &capture::checkpoint_ref(&id)?, "0/full.jsonl")?
                    == c.files["0/full.jsonl"]
        }
        _ => anyhow::bail!("Unknown hosted check"),
    };
    ensure!(valid, "Hosted assertion failed: {}", description(index));
    Ok(())
}
fn main() {
    let mut args = Arguments::from_args();
    args.test_threads = Some(1);
    let records = Arc::new(Mutex::new(vec![]));
    let mut trials = vec![];
    for (profile, name) in PROFILES.iter().enumerate() {
        let fixture = Arc::new(OnceLock::<std::result::Result<Case, String>>::new());
        for (index, description) in CHECKS.iter().enumerate() {
            let description = if codex() {
                crate::description(index)
            } else {
                description
            };
            let name = format!(
                "{}-cloud/{name}: {description}",
                if codex() { "codex" } else { "claude" }
            );
            let fixture = fixture.clone();
            let records = records.clone();
            trials.push(Trial::test(name.clone(), move || {
                let result =
                    match fixture.get_or_init(|| scenario(profile).map_err(|e| format!("{e:#}"))) {
                        Ok(c) => check(index, c).map_err(|e| format!("{e:#}")),
                        Err(e) => Err(e.clone()),
                    };
                records.lock().expect("hosted results").push(
                    json!({"name":name,"passed":result.is_ok(),"error":result.as_ref().err()}),
                );
                result.map_err(Into::into)
            }));
        }
    }
    assert_eq!(trials.len(), 1000);
    let result = libtest_mimic::run(&args, trials);
    let rows = records.lock().expect("hosted results");
    support::report(if codex() {"codex-hosted-contract.json"} else {"claude-hosted-contract.json"},&json!({"provider":format!("{}-cloud",provider()),"planned_tests":1000,
        "executed_tests":rows.len(),"passed":rows.iter().filter(|r|r["passed"]==true).count(),
        "qualification":if codex() {"20 synthetic native app-server API/privacy scenarios x 50 assertions; not 1,000 live hosted sessions"} else {"20 synthetic hosted native transcript/handoff scenarios x 50 assertions; not 1,000 live hosted sessions"},
        "timestamp":chrono::Utc::now().to_rfc3339(),"results":*rows})).expect("Cannot persist hosted results");
    result.exit();
}
