mod support;

use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use support::{Fixture, put};
use trailvault::{capture, git, native::Provider, redact};

const CHECKS: &[&str] = &[
    "native session identity retained",
    "provider identity retained",
    "source worktree verified",
    "native file unchanged by capture and publication",
    "native file permissions unchanged",
    "pending copy is private",
    "pending data excluded from source",
    "source tree untouched by snapshot",
    "pre-push snapshot has own orphan ref",
    "idle snapshot retains explicit turn boundary",
    "ended snapshot is not task completion",
    "published native JSONL is valid",
    "compact header identifies provider",
    "compact user text retained",
    "compact assistant text retained",
    "tool calls have native IDs",
    "tool outcomes retained",
    "hidden failed child is not wrapper success",
    "native unknown events retained",
    "native usage and metadata retained",
    "native observation offsets valid",
    "PR references canonicalized",
    "same-number PRs in different repos remain separate",
    "references do not fabricate views",
    "references do not fabricate contributions",
    "lifecycle snapshots are historical not live leases",
    "dirty files captured independently",
    "native content hash exact",
    "secrets redacted before publication",
    "prompt privacy matches transcript policy",
    "source commit links only observed changed files",
    "source history excludes checkpoint commits",
    "snapshot metadata indexes evidence",
    "source native evidence frozen across later boundaries",
    "normal Git push publishes queued refs",
    "fresh clone fetches checkpoint explicitly",
    "every fetched file is byte-identical",
    "upload queue drained after success",
    "failed push retains durable queue",
    "manual publication works with automatic uploads disabled",
    "missing identity fails explicitly",
    "invalid identity fails explicitly",
    "different provider cannot take over session",
    "external transcript rejected",
    "relative transcript rejected",
    "malformed native JSON rejected",
    "wrong native session rejected",
    "wrong native worktree rejected",
    "disabled capture cannot publish old snapshot",
    "no-code session can publish without inventing source changes",
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
    "mentioned-only",
    "nested-failure",
];

fn text(profile: usize) -> String {
    match profile {
        1 => "fictional route with spaces".into(),
        2 => "caf\u{e9} \u{65e5}\u{672c}".into(),
        3 => "line one\nline two".into(),
        4 => "\"quoted\" 'single' \\slash".into(),
        5 => "$(touch NEVER_EXECUTE); echo literal".into(),
        6 => "<script>inert()</script>".into(),
        7 => "fictional native output ".repeat(512),
        8 => "DB_PASSWORD=synthetic-native-fixture-72849".into(),
        9 => "PRIVATE-NATIVE-123456".into(),
        10 => "alex.privacy@example.invalid".into(),
        _ => format!("Synthetic native fixture profile {profile}"),
    }
}
fn transcript(
    provider: Provider,
    id: &str,
    cwd: &Path,
    profile: usize,
    complete: bool,
) -> Result<String> {
    let content = format!(
        "{} https://github.com/madsbolaris/trailvault-capture-test/pull/1 https://github.com/entireio/cli/pull/1",
        text(profile)
    );
    let output = if profile == 19 {
        serde_json::to_string(&json!([{"type":"input_text","text":"Script completed"},
            {"type":"input_text","text":"{\"exit_code\":128,\"output\":\"Operation not permitted\"}"}]))?
    } else {
        format!(
            "{{\"number\":18,\"url\":\"https://github.com/madsbolaris/trailvault-capture-test/pull/18\",\"title\":{}}}",
            serde_json::to_string(&text(profile))?
        )
    };
    let mut events = if provider == Provider::Claude {
        vec![
            json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"role":"user","content":content}}),
            json!({"type":"assistant","sessionId":id,"cwd":cwd,"message":{"role":"assistant","model":"fixture-model","usage":{"input_tokens":12},
                "content":[{"type":"text","text":text(profile)},{"type":"tool_use","id":"native-call-1","name":"Bash","input":{"command":"gh pr view 18 --repo madsbolaris/trailvault-capture-test"}}]}}),
        ]
    } else {
        vec![
            json!({"type":"session_meta","payload":{"id":id,"cwd":cwd,"originator":"codex_exec"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":content}]}}),
            json!({"type":"turn_context","payload":{"model":"fixture-model","usage":{"input_tokens":12}}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text(profile)}]}}),
            json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"native-call-1","name":"exec",
                "input":"text(await tools.exec_command({cmd:\"gh pr view 18 --repo madsbolaris/trailvault-capture-test\"}));"}}),
        ]
    };
    if profile == 13 {
        events.push(if provider==Provider::Claude {
            json!({"type":"assistant","sessionId":id,"cwd":cwd,"message":{"content":[{"type":"tool_use","id":"parallel-2","name":"Read","input":{"file_path":"tracked.txt"}}]}})
        } else {json!({"type":"response_item","payload":{"type":"function_call","call_id":"parallel-2","name":"read_file","arguments":"{\"path\":\"tracked.txt\"}"}})});
    }
    if complete {
        events.push(if provider==Provider::Claude {
            json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":[{"type":"tool_result","tool_use_id":"native-call-1","is_error":profile==11||profile==12,"content":output}]}})
        } else {json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"native-call-1","is_error":profile==11||profile==12,"output":output}})});
        if profile == 13 {
            events.push(if provider==Provider::Claude {
                json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":[{"type":"tool_result","tool_use_id":"parallel-2","content":"read complete","is_error":false}]}})
            } else {json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"parallel-2","output":"read complete"}})});
        }
        events.push(json!({"type":"future.native","sessionId":id,"value":{"text":text(profile)}}));
        if provider == Provider::Claude {
            events.push(json!({"type":"pr-link","sessionId":id,"prUrl":"https://github.com/madsbolaris/trailvault-capture-test/pull/18","prNumber":18,"prRepository":"madsbolaris/trailvault-capture-test"}));
        }
    }
    let raw = redact::jsonl(&events)?;
    Ok(if profile == 17 {
        raw.replace('\n', "\r\n")
    } else if profile == 16 {
        raw.trim_end().into()
    } else {
        raw
    })
}
fn blob(cwd: &Path, reference: &str, path: &str) -> Result<String> {
    let output = git::command(
        cwd,
        Path::new("git"),
        &["show", &format!("{reference}:{path}")],
        &BTreeMap::new(),
        None,
        Duration::from_secs(30),
    )?;
    ensure!(
        output.success,
        "Cannot read checkpoint file: {}",
        output.stderr
    );
    Ok(output.stdout)
}
fn native(
    f: &Fixture,
    provider: Provider,
    boundary: &str,
    payload: &Value,
    checkpoint: bool,
) -> Result<String> {
    let mut args = vec!["capture", provider.name(), boundary, "-"];
    if checkpoint {
        args.push("--checkpoint");
    }
    support::invoke(&f.repo, &args, &f.env, Some(payload))
}
struct Case {
    f: Fixture,
    provider: Provider,
    profile: usize,
    id: String,
    path: PathBuf,
    raw: String,
    mode: u32,
    reference: String,
    active: String,
    idle: String,
    full: String,
    compact: Vec<Value>,
    evidence: Value,
    metadata: Value,
    root: Value,
    source: String,
    tree: String,
    remote: PathBuf,
    fresh: PathBuf,
    files: BTreeMap<String, String>,
    fetched: BTreeMap<String, String>,
}
fn scenario(provider: Provider, profile: usize) -> Result<Case> {
    let mut f = Fixture::new()?;
    let remote = f.remote()?;
    let mut settings = json!({"enabled":true,"commit_linking":"always"});
    if profile == 9 {
        settings["redaction"] = json!({"custom_redactions":{"fixture":"PRIVATE-NATIVE-[0-9]+"}});
    }
    if profile == 10 {
        settings["redaction"] = json!({"pii":{"enabled":true}});
    }
    f.configure(&settings)?;
    let home = f.temp.path().join("native-home");
    f.env.insert(
        if provider == Provider::Claude {
            "CLAUDE_CONFIG_DIR"
        } else {
            "CODEX_HOME"
        }
        .into(),
        support::text(&home),
    );
    let id = uuid::Uuid::new_v4().to_string();
    let path = if provider == Provider::Claude {
        home.join("projects/fixture").join(format!("{id}.jsonl"))
    } else {
        home.join("sessions/2026/11/03")
            .join(format!("rollout-fixture-{id}.jsonl"))
    };
    let payload = json!({"sessionId":id,"transcriptPath":path});
    native(&f, provider, "session-start", &payload, false)?;
    let raw = transcript(provider, &id, &f.repo, profile, true)?;
    let mode = 0o600;
    put(&path, transcript(provider, &id, &f.repo, profile, false)?)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
    if profile != 18 {
        put(
            &f.repo.join("agent change.txt"),
            "observed native agent change\n",
        )?;
    }
    let source = f.g(&["rev-parse", "HEAD"])?;
    let tree = f.g(&["write-tree"])?;
    let active = capture::checkpoint_ref(&native(&f, provider, "post-tool-use", &payload, true)?)?;
    put(&path, &raw)?;
    let idle = capture::checkpoint_ref(&native(&f, provider, "agent-stop", &payload, true)?)?;
    let ended = capture::checkpoint_ref(&native(&f, provider, "session-end", &payload, true)?)?;
    let reference = if profile != 18 {
        f.g(&["add", "agent change.txt"])?;
        f.g(&["commit", "-m", "Observed native source change"])?;
        capture::checkpoint_ref(
            &capture::trailer(
                &f.g(&["log", "-1", "--format=%B"])?,
                "Trailvault-Checkpoint",
            )
            .context("Missing native commit checkpoint")?,
        )?
    } else {
        ended
    };
    let full = blob(&f.repo, &reference, "0/full.jsonl")?;
    let compact = redact::parse(&blob(&f.repo, &reference, "0/transcript.jsonl")?)?;
    let evidence = serde_json::from_str(&blob(&f.repo, &reference, "0/evidence.json")?)?;
    let metadata = serde_json::from_str(&blob(&f.repo, &reference, "0/metadata.json")?)?;
    let root = serde_json::from_str(&blob(&f.repo, &reference, "metadata.json")?)?;
    let paths = f.g(&["ls-tree", "-r", "--name-only", &reference])?;
    let files = paths
        .lines()
        .map(|p| Ok((p.into(), blob(&f.repo, &reference, p)?)))
        .collect::<Result<_>>()?;
    f.g(&["push", "-u", "origin", "main"])?;
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
    let output = git::command(
        &fresh,
        &support::executable()?,
        &["fetch", "origin"],
        &f.env,
        None,
        Duration::from_secs(30),
    )?;
    ensure!(
        output.success,
        "Fresh clone fetch failed: {}",
        output.stderr
    );
    let fetched = paths
        .lines()
        .map(|p| Ok((p.into(), blob(&fresh, &reference, p)?)))
        .collect::<Result<_>>()?;
    Ok(Case {
        f,
        provider,
        profile,
        id,
        path,
        raw,
        mode,
        reference,
        active,
        idle,
        full,
        compact,
        evidence,
        metadata,
        root,
        source,
        tree,
        remote,
        fresh,
        files,
        fetched,
    })
}
fn invalid(c: &Case, change: impl FnOnce(&mut Value), expected: &str) -> Result<()> {
    let mut payload = json!({"sessionId":c.id,"transcriptPath":c.path});
    change(&mut payload);
    let error = native(&c.f, c.provider, "post-tool-use", &payload, false)
        .expect_err("Invalid capture succeeded");
    ensure!(
        format!("{error:#}").contains(expected),
        "Unexpected rejection: {error:#}"
    );
    Ok(())
}
fn invalid_raw(c: &Case, raw: &str, expected: &str) -> Result<()> {
    put(&c.path, raw)?;
    let result = invalid(c, |_| {}, expected);
    put(&c.path, &c.raw)?;
    result
}
fn check(index: usize, c: &Case) -> Result<()> {
    let p = &c.evidence;
    let refs = p["pr_references"]
        .as_array()
        .context("Missing PR references")?;
    let observations = p["observations"]
        .as_array()
        .context("Missing observations")?;
    let valid = match index {
        0 => p["session_id"] == c.id,
        1 => p["provider"] == c.provider.name() && c.metadata["agent"] == c.provider.label(),
        2 => observations.iter().any(|e| {
            e["event"]["cwd"] == json!(c.f.repo) || e["event"]["payload"]["cwd"] == json!(c.f.repo)
        }),
        3 => fs::read_to_string(&c.path)? == c.raw,
        4 => fs::metadata(&c.path)?.permissions().mode() & 0o777 == c.mode,
        5 => {
            fs::metadata(
                c.f.repo
                    .join(".trailvault/metadata")
                    .join(&c.id)
                    .join("full.jsonl"),
            )?
            .permissions()
            .mode()
                & 0o777
                == 0o600
        }
        6 => {
            c.f.g(&[
                "check-ignore",
                &format!(".trailvault/metadata/{}/full.jsonl", c.id),
            ])?
            .contains("metadata")
        }
        7 => c.tree == c.f.g(&["rev-parse", &format!("{}^{{tree}}", c.source)])?,
        8 => {
            c.f.g(&["rev-list", "--parents", "-1", &c.active])?
                .split_whitespace()
                .count()
                == 1
        }
        9 => {
            serde_json::from_str::<Value>(&blob(&c.f.repo, &c.idle, "0/evidence.json")?)?["lifecycle"]
                ["state"]
                == "idle"
        }
        10 => {
            p["lifecycle"]["state"] == "ended" && p["lifecycle"]["task_completion"] == "unverified"
        }
        11 => !redact::parse(&c.full)?.is_empty(),
        12 => c.compact[0]["agent"] == c.provider.label(),
        13 => c.compact.iter().any(|e| e["role"] == "user"),
        14 => c.compact.iter().any(|e| e["role"] == "assistant"),
        15 => observations
            .iter()
            .any(|e| e["kind"] == "tool.started" && e["call_id"] == "native-call-1"),
        16 => observations
            .iter()
            .any(|e| e["kind"] == "tool.finished" && e["call_id"] == "native-call-1"),
        17 => observations.iter().any(|e| {
            e["kind"] == "tool.finished"
                && e["status"]
                    == if [11, 12, 19].contains(&c.profile) {
                        "failed"
                    } else {
                        "returned"
                    }
        }),
        18 => redact::parse(&c.full)?
            .iter()
            .any(|e| e["type"] == "future.native"),
        19 => c.full.contains("fixture-model") && c.full.contains("input_tokens"),
        20 => observations.iter().all(|e| {
            e["native_line"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= redact::parse(&c.full).map_or(0, |v| v.len()) as u64)
        }),
        21 => {
            refs.iter().all(|r| {
                r["url"]
                    == format!(
                        "https://github.com/{}/pull/{}",
                        r["repository"].as_str().unwrap_or(""),
                        r["number"]
                    )
            }) && refs.iter().any(|r| {
                r["url"] == "https://github.com/madsbolaris/trailvault-capture-test/pull/18"
            }) == (c.provider == Provider::Claude || c.profile != 19)
        }
        22 => {
            refs.iter().any(|r| {
                r["repository"] == "madsbolaris/trailvault-capture-test" && r["number"] == 1
            }) && refs
                .iter()
                .any(|r| r["repository"] == "entireio/cli" && r["number"] == 1)
        }
        23 => refs.iter().all(|r| r["retrieval_verified"] == false),
        24 => refs.iter().all(|r| r["contribution_verified"] == false),
        25 => {
            p["lifecycle"]["presence"] == "historical_snapshot"
                && p["lifecycle"]["live_lease"] == false
        }
        26 => {
            p["dirty_files"]
                .as_array()
                .context("No dirty files")?
                .iter()
                .any(|p| p == "agent change.txt")
                == (c.profile != 18)
        }
        27 => {
            c.files["0/content_hash.txt"] == format!("sha256:{}\n", capture::hash(&c.full))
                && p["native_sha256"] == capture::hash(&c.full)
        }
        28 => {
            let references = c.f.g(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/trailvault/checkpoints/",
            ])?;
            let mut valid = true;
            let literal = serde_json::to_string(&text(c.profile))?;
            let needle = &literal[1..literal.len() - 1];
            for reference in references.lines() {
                valid &= blob(&c.f.repo, reference, "0/full.jsonl")?.contains(needle)
                    == ![8, 9, 10].contains(&c.profile);
                if [8, 9, 10].contains(&c.profile) {
                    for path in c.f.g(&["ls-tree", "-r", "--name-only", reference])?.lines() {
                        valid &= !blob(&c.f.repo, reference, path)?.contains(&text(c.profile));
                    }
                }
            }
            valid
        }
        29 => {
            c.files["0/prompt.txt"].contains(&text(c.profile)) == ![8, 9, 10].contains(&c.profile)
        }
        30 => {
            if c.profile == 18 {
                c.f.g(&["rev-parse", "HEAD"])? == c.source
            } else {
                c.metadata["files_touched"]
                    .as_array()
                    .context("No touched files")?
                    .iter()
                    .any(|p| p == "agent change.txt")
            }
        }
        31 => !c.f.g(&["rev-list", "HEAD"])?.lines().any(|line| {
            c.f.g(&["rev-parse", &c.reference])
                .is_ok_and(|sha| sha == line)
        }),
        32 => c.root["sessions"][0]["evidence"] == "/0/evidence.json",
        33 => {
            serde_json::from_str::<Value>(&blob(&c.f.repo, &c.active, "0/evidence.json")?)?["lifecycle"]
                ["state"]
                == "working"
                && !blob(&c.f.repo, &c.active, "0/full.jsonl")?.contains("future.native")
        }
        34 => {
            support::g(
                c.f.temp.path(),
                &[
                    &format!("--git-dir={}", c.remote.display()),
                    "rev-parse",
                    &c.reference,
                ],
            )? == c.f.g(&["rev-parse", &c.reference])?
        }
        35 => {
            support::g(&c.fresh, &["rev-parse", &c.reference])?
                == c.f.g(&["rev-parse", &c.reference])?
        }
        36 => c.files == c.fetched,
        37 => git::read::<Value>(&c.f.repo.join(".git/trailvault/push-queue.json"))? == json!([]),
        38 | 39 => {
            let payload = json!({"sessionId":c.id,"transcriptPath":c.path});
            let id = native(&c.f, c.provider, "session-end", &payload, true)?;
            if index == 38 {
                let result = c.f.cli(&[
                    "push",
                    support::text(&c.f.temp.path().join("missing-remote")).as_str(),
                ]);
                ensure!(
                    result.is_err(),
                    "Missing destination unexpectedly succeeded"
                );
                let queued: Vec<String> =
                    git::read(&c.f.repo.join(".git/trailvault/push-queue.json"))?;
                ensure!(
                    queued.contains(&capture::checkpoint_ref(&id)?),
                    "Failed upload lost queue"
                );
            } else {
                let mut settings = capture::settings(&git::Repo::discover(&c.f.repo)?)?;
                settings["strategy_options"] = json!({"push_sessions":false});
                c.f.configure(&settings)?;
            }
            c.f.cli(&["push", "origin"])?;
            true
        }
        40 => {
            invalid(
                c,
                |p| {
                    p.as_object_mut().unwrap().remove("sessionId");
                },
                "sessionId is required",
            )?;
            true
        }
        41 => {
            invalid(
                c,
                |p| p["sessionId"] = json!("../invalid"),
                "must be a UUID",
            )?;
            true
        }
        42 => {
            let other = if c.provider == Provider::Claude {
                Provider::Codex
            } else {
                Provider::Claude
            };
            let error = native(
                &c.f,
                other,
                "session-start",
                &json!({"sessionId":c.id}),
                false,
            )
            .expect_err("Provider takeover succeeded");
            ensure!(format!("{error:#}").contains("different worktree/provider"));
            true
        }
        43 => {
            let external = c.f.temp.path().join("external.jsonl");
            put(&external, &c.raw)?;
            invalid(c, |p| p["transcriptPath"] = json!(external), "inside")?;
            true
        }
        44 => {
            invalid(
                c,
                |p| p["transcriptPath"] = json!("relative.jsonl"),
                "must be absolute",
            )?;
            true
        }
        45 => {
            invalid_raw(c, "{not-json}\n", "Invalid transcript line")?;
            true
        }
        46 => {
            let wrong = c.raw.replace(&c.id, "00000000-0000-4000-8000-000000000001");
            invalid_raw(c, &wrong, "different session identity")?;
            true
        }
        47 => {
            let wrong = c
                .raw
                .replace(&support::text(&c.f.repo), &support::text(c.f.temp.path()));
            invalid_raw(c, &wrong, "different worktree")?;
            true
        }
        48 => {
            let before = c.f.g(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/trailvault/checkpoints/",
            ])?;
            let settings = capture::settings(&git::Repo::discover(&c.f.repo)?)?;
            let mut disabled = settings.clone();
            disabled["enabled"] = json!(false);
            c.f.configure(&disabled)?;
            let output = native(
                &c.f,
                c.provider,
                "session-end",
                &json!({"sessionId":c.id,"transcriptPath":c.path}),
                true,
            );
            c.f.configure(&settings)?;
            ensure!(output.is_err(), "Disabled snapshot publication succeeded");
            ensure!(
                c.f.g(&[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/trailvault/checkpoints/"
                ])? == before
            );
            true
        }
        49 => {
            p["publication"]["contribution_verified"] == false
                && (c.profile != 18 || c.metadata["strategy"] == "session-snapshot")
        }
        _ => anyhow::bail!("Unknown native assertion"),
    };
    ensure!(valid, "Native contract assertion failed: {}", CHECKS[index]);
    Ok(())
}
fn main() {
    let mut args = Arguments::from_args();
    args.test_threads = Some(1);
    let records = Arc::new(Mutex::new(Vec::new()));
    let mut trials = vec![];
    for provider in [Provider::Claude, Provider::Codex] {
        for (profile, name) in PROFILES.iter().enumerate() {
            let fixture = Arc::new(OnceLock::<std::result::Result<Case, String>>::new());
            for (index, description) in CHECKS.iter().enumerate() {
                let name = format!("{}/{name}: {description}", provider.name());
                let fixture = fixture.clone();
                let records = records.clone();
                trials.push(Trial::test(name.clone(), move || {
                    let result = match fixture
                        .get_or_init(|| scenario(provider, profile).map_err(|e| format!("{e:#}")))
                    {
                        Ok(case) => check(index, case).map_err(|e| format!("{e:#}")),
                        Err(error) => Err(error.clone()),
                    };
                    records
                        .lock()
                        .expect("native results lock")
                        .push(json!({"provider":provider.name(),"name":name,
                        "passed":result.is_ok(),"error":result.as_ref().err()}));
                    result.map_err(Into::into)
                }));
            }
        }
    }
    assert_eq!(CHECKS.len() * PROFILES.len(), 1000);
    assert_eq!(trials.len(), 2000);
    let result = libtest_mimic::run(&args, trials);
    let records = records.lock().expect("native results lock");
    for provider in [Provider::Claude, Provider::Codex] {
        let rows: Vec<_> = records
            .iter()
            .filter(|r| r["provider"] == provider.name())
            .collect();
        support::report(&format!("{}-native-contract.json",provider.name()),&json!({"provider":provider.name(),
            "qualification":"20 deterministic native transcript scenarios x 50 assertions; not 1,000 live model sessions",
            "planned_tests":1000,"executed_tests":rows.len(),"passed":rows.iter().filter(|r|r["passed"]==true).count(),
            "timestamp":chrono::Utc::now().to_rfc3339(),"results":rows})).expect("Cannot persist native contract results");
    }
    result.exit();
}
