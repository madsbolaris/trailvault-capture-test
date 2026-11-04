mod support;

use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use support::{Fixture, event, put};
use trailvault::{capture, git, install, redact};
use uuid::Uuid;

// These run local subprocesses with cloud-shaped inputs. They do not impersonate
// a hosted Copilot run; hosted evidence is recorded separately.
const CASES: &[&str] = &[
    "repo-hook-schema",
    "all-required-hooks",
    "bash-entrypoint",
    "executable-available",
    "hook-timeout-budget",
    "no-hook-credentials",
    "setup-idempotent",
    "source-tree-clean",
    "session-start-state",
    "cloud-session-id",
    "prompt-captured",
    "native-log-untouched",
    "native-private-copy",
    "agent-stop-records-files",
    "pretool-does-not-end-turn",
    "posttool-does-not-end-turn",
    "error-hook-does-not-end-turn",
    "session-end-state",
    "noninteractive-commit-links",
    "orphan-checkpoint",
    "metadata-index",
    "native-events-roundtrip",
    "compact-events-roundtrip",
    "tool-start-retained",
    "tool-result-retained",
    "content-hash-exact",
    "redaction-before-storage",
    "source-history-separated",
    "publication-queued",
    "push-publishes-custom-ref",
    "queue-drained",
    "fresh-clone-fetch",
    "no-visible-checkpoint-branch",
    "missing-executable-explicit-failure",
    "missing-transcript-explicit-failure",
    "external-transcript-rejected",
    "malformed-transcript-rejected",
    "missing-identity-rejected",
    "invalid-identity-rejected",
    "late-start-cannot-invent-attribution",
    "failed-upload-retained",
    "disabled-capture-skipped",
    "local-only-not-published",
    "no-verify-does-not-prove-linkage",
    "no-code-changes-no-checkpoint",
    "relative-transcript-rejected",
    "missing-repo-explicit-failure",
    "bad-hook-event-rejected",
    "invalid-json-payload-rejected",
    "policy-refusal-no-capture",
];
const PROFILES: &[&str] = &[
    "plain",
    "spaces",
    "unicode",
    "multiline",
    "quotes",
    "shell-literal",
    "markdown",
    "html-literal",
    "long-output",
    "null-result",
    "failed-tool",
    "denied-tool",
    "parallel-tools",
    "unknown-native-event",
    "usage",
    "model-change",
    "resumed-turn",
    "crlf",
    "no-final-newline",
    "secret-shaped",
];

fn data(profile: usize) -> String {
    match profile {
        1 => "route with spaces".into(),
        2 => "caf\u{e9} \u{65e5}\u{672c}".into(),
        3 => "line one\nline two".into(),
        4 => "\"quoted\" 'single' \\backslash".into(),
        5 => "$(touch NEVER_EXECUTE); echo literal".into(),
        6 => "```rust\nfn main() {}\n```".into(),
        7 => "<script>inert()</script>".into(),
        8 => "fictional cloud output ".repeat(512),
        19 => "DB_PASSWORD=synthetic-cloud-fixture-72849".into(),
        _ => format!("Fictional cloud test profile {profile}"),
    }
}

struct Cloud {
    f: Fixture,
    payload: Value,
    id: String,
    path: PathBuf,
    events: Vec<Value>,
}

impl Cloud {
    fn new(profile: usize) -> Result<Self> {
        let f = Fixture::new()?;
        let id = Uuid::new_v4().to_string();
        let path = PathBuf::from(&f.env["COPILOT_HOME"])
            .join("session-state")
            .join(&id)
            .join("events.jsonl");
        let text = data(profile);
        let mut events = vec![
            event(
                "session.start",
                json!({"sessionId":id,"version":1,"producer":"copilot-agent","copilotVersion":"1.0.105","context":{"cwd":f.repo}}),
            ),
            event("user.message", json!({"content":text})),
            event(
                "assistant.message",
                json!({"content":"Cloud-shaped local fixture; not a hosted run."}),
            ),
            event(
                "tool.execution_start",
                json!({"toolCallId":"call-one","toolName":"shell","arguments":{"command":text}}),
            ),
            event(
                "tool.execution_complete",
                json!({"toolCallId":"call-one","success":!matches!(profile,10|11),"result":if profile==9 {Value::Null} else {json!({"content":text})},"error":if profile==11 {json!({"message":"permission denied"})} else {Value::Null}}),
            ),
        ];
        match profile {
            12 => {
                events.insert(4,event("tool.execution_start",json!({"toolCallId":"call-two","toolName":"view","arguments":{"path":"tracked.txt"}})));
                events.push(event("tool.execution_complete",json!({"toolCallId":"call-two","success":true,"result":{"content":"parallel output"}})));
            }
            13 => events.push(event(
                "future.event",
                json!({"content":"preserve unknown native payload"}),
            )),
            14 => events.push(event(
                "session.usage_record",
                json!({"inputTokens":53,"outputTokens":21}),
            )),
            15 => events.push(event(
                "session.model_change",
                json!({"model":"recorded-test-model"}),
            )),
            16 => events.push(event(
                "user.message",
                json!({"content":"Continue the same fictional task."}),
            )),
            _ => {}
        }
        let mut full = redact::jsonl(&events)?;
        if profile == 17 {
            full = full.replace('\n', "\r\n");
        }
        if profile == 18 {
            full = full.trim_end().into();
        }
        put(&path, full)?;
        let payload = json!({"sessionId":id,"cwd":f.repo,"transcriptPath":path,"prompt":text,"initialPrompt":text,"timestamp":chrono::Utc::now().timestamp_millis(),"source":"new"});
        Ok(Self {
            f,
            payload,
            id,
            path,
            events,
        })
    }
    fn start(&self) -> Result<()> {
        self.f.hook("session-start", &self.payload)?;
        self.f.hook("user-prompt-submitted", &self.payload)?;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        put(
            &self.f.repo.join("cloud change.txt"),
            "fictional cloud change\n",
        )?;
        self.f.hook("agent-stop", &self.payload)?;
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
    fn commit(&self) -> Result<(String, String)> {
        self.f.g(&["add", "cloud change.txt"])?;
        self.f
            .g(&["commit", "-m", "Cloud-shaped capture fixture"])?;
        let message = self.f.g(&["log", "-1", "--format=%B"])?;
        let id = capture::trailer(&message, "Trailvault-Checkpoint")
            .context("Missing checkpoint trailer")?;
        let reference = capture::checkpoint_ref(&id)?;
        let sha = self.f.g(&["rev-parse", &reference])?;
        Ok((reference, sha))
    }
    fn artifact(&self, sha: &str, path: &str) -> Result<String> {
        let output = git::run(
            &self.f.repo,
            &["show", &format!("{sha}:{path}")],
            &self.f.env,
            None,
        )?;
        ensure!(output.success, "Artifact read failed: {}", output.stderr);
        Ok(output.stdout)
    }
}

fn run(case: usize, profile: usize) -> Result<()> {
    let c = Cloud::new(profile)?;
    if case <= 7 {
        let hook: Value = git::read(&c.f.repo.join(".github/hooks/trailvault.json"))?;
        let binary = support::executable()?.canonicalize()?;
        match case {
            0 => ensure!(hook["version"] == 1 && hook["hooks"].is_object()),
            1 => {
                for (name, _) in install::EVENTS {
                    ensure!(
                        hook["hooks"][name]
                            .as_array()
                            .is_some_and(|a| !a.is_empty())
                    );
                }
            }
            2 => {
                for (name, command) in install::EVENTS {
                    ensure!(
                        hook["hooks"][name][0]["bash"]
                            .as_str()
                            .context("bash")?
                            .ends_with(&format!("hooks copilot-cli {command}"))
                    );
                }
            }
            3 => {
                ensure!(binary.is_file() && fs::metadata(binary)?.permissions().mode() & 0o111 != 0)
            }
            4 => {
                for (name, _) in install::EVENTS {
                    ensure!(
                        hook["hooks"][name][0]
                            .get("timeoutSec")
                            .is_none_or(|value| value
                                .as_u64()
                                .is_some_and(|timeout| timeout <= 30))
                    );
                }
            }
            5 => {
                let text = serde_json::to_string(&hook)?;
                ensure!(
                    !text.contains("GITHUB_COPILOT_GIT_TOKEN")
                        && !text.contains("GH_TOKEN")
                        && !text.contains(&data(19))
                );
            }
            6 => {
                let before = fs::read(c.f.repo.join(".github/hooks/trailvault.json"))?;
                c.f.cli(&[
                    "enable",
                    "--agent",
                    "copilot-cli",
                    "--absolute-git-hook-path",
                    "--telemetry=false",
                ])?;
                ensure!(before == fs::read(c.f.repo.join(".github/hooks/trailvault.json"))?);
            }
            7 => ensure!(c.f.g(&["status", "--porcelain"])?.is_empty()),
            _ => unreachable!(),
        }
        return Ok(());
    }
    if case >= 33 {
        match case {
            33 => {
                let error = git::command(
                    &c.f.repo,
                    &c.f.temp.path().join("not-installed-trailvault"),
                    &["hooks", "copilot-cli", "session-start"],
                    &c.f.env,
                    None,
                    std::time::Duration::from_secs(5),
                )
                .err()
                .context("Missing binary unexpectedly ran")?;
                ensure!(format!("{error:#}").contains("Cannot start"));
            }
            34 => {
                c.start()?;
                fs::remove_file(&c.path)?;
                ensure!(c.stop().is_err());
            }
            35 => {
                c.start()?;
                let outside = c.f.temp.path().join("outside.jsonl");
                put(&outside, redact::jsonl(&c.events)?)?;
                let mut payload = c.payload.clone();
                payload["transcriptPath"] = json!(outside);
                ensure!(c.f.hook("agent-stop", &payload).is_err());
            }
            36 => {
                c.start()?;
                put(&c.path, "{broken JSON\n")?;
                ensure!(c.stop().is_err());
            }
            37 => {
                let mut payload = c.payload.clone();
                payload
                    .as_object_mut()
                    .context("payload")?
                    .remove("sessionId");
                ensure!(c.f.hook("session-start", &payload).is_err());
            }
            38 => {
                let mut payload = c.payload.clone();
                payload["sessionId"] = json!("../escape");
                ensure!(c.f.hook("session-start", &payload).is_err());
            }
            39 => {
                put(
                    &c.f.repo.join("already-changed.txt"),
                    "Changed before capture started\n",
                )?;
                c.f.hook("agent-stop", &c.payload)?;
                ensure!(
                    c.state()?.files_touched.is_empty(),
                    "Late capture must not invent an earlier baseline"
                );
            }
            40 => {
                c.start()?;
                c.stop()?;
                c.commit()?;
                c.f.g(&[
                    "remote",
                    "add",
                    "origin",
                    support::text(&c.f.temp.path().join("missing-remote")).as_str(),
                ])?;
                ensure!(c.f.cli(&["push", "origin"]).is_err());
                let queue: Vec<String> =
                    git::read(&c.f.repo.join(".git/trailvault/push-queue.json"))?;
                ensure!(!queue.is_empty());
            }
            41 => {
                c.f.cli(&["disable"])?;
                c.f.hook("session-start", &c.payload)?;
                ensure!(!c.f.repo.join(".git/trailvault-sessions").exists());
            }
            42 => {
                c.f.configure(
                    &json!({"strategy_options":{"push_sessions":false},"commit_linking":"always"}),
                )?;
                let remote = c.f.remote()?;
                c.start()?;
                c.stop()?;
                let (reference, _) = c.commit()?;
                c.f.g(&["push", "origin", "main"])?;
                ensure!(
                    !git::run(
                        c.f.temp.path(),
                        &[
                            "--git-dir",
                            support::text(&remote).as_str(),
                            "rev-parse",
                            "--verify",
                            &reference
                        ],
                        &c.f.env,
                        None
                    )?
                    .success
                );
                let queue: Vec<String> =
                    git::read(&c.f.repo.join(".git/trailvault/push-queue.json"))?;
                ensure!(
                    queue.contains(&reference),
                    "Local-only policy must retain retryable local checkpoints"
                );
            }
            43 => {
                c.start()?;
                c.stop()?;
                c.f.g(&["add", "cloud change.txt"])?;
                c.f.g(&[
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "-m",
                    "Bypass local capture",
                ])?;
                ensure!(
                    !c.f.g(&["log", "-1", "--format=%B"])?
                        .contains("Trailvault-Checkpoint:")
                );
            }
            44 => {
                c.start()?;
                c.f.hook("agent-stop", &c.payload)?;
                c.f.g(&["commit", "--allow-empty", "-m", "No changed source"])?;
                ensure!(
                    !c.f.g(&["log", "-1", "--format=%B"])?
                        .contains("Trailvault-Checkpoint:")
                );
            }
            45 => {
                c.start()?;
                let mut payload = c.payload.clone();
                payload["transcriptPath"] = json!("relative/events.jsonl");
                ensure!(c.f.hook("agent-stop", &payload).is_err());
            }
            46 => {
                ensure!(
                    support::invoke(
                        c.f.temp.path(),
                        &["hooks", "copilot-cli", "session-start"],
                        &c.f.env,
                        Some(&c.payload)
                    )
                    .is_err()
                );
            }
            47 => {
                ensure!(c.f.hook("not-a-supported-event", &c.payload).is_err());
            }
            48 => {
                let result = git::command(
                    &c.f.repo,
                    &support::executable()?,
                    &["hooks", "copilot-cli", "session-start"],
                    &c.f.env,
                    Some(b"{not-json"),
                    std::time::Duration::from_secs(10),
                )?;
                ensure!(!result.success && !result.stderr.is_empty());
            }
            49 => {
                put(
                    &c.f.repo.join(".trailvault/policy.json"),
                    serde_json::to_vec(
                        &json!({"version":1,"logging":"required","destination":"origin"}),
                    )?,
                )?;
                c.f.g(&["add", ".trailvault/policy.json"])?;
                c.f.g(&[
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "-m",
                    "Declare policy without consent",
                ])?;
                c.f.remote()?;
                c.f.hook("session-start", &c.payload)?;
                ensure!(!c.f.repo.join(".git/trailvault-sessions").exists());
            }
            _ => unreachable!(),
        }
        return Ok(());
    }
    c.start()?;
    let native = fs::read(&c.path)?;
    if case == 8 {
        ensure!(c.state()?.phase == "active");
        return Ok(());
    }
    if case == 9 {
        ensure!(c.state()?.session_id == c.id);
        return Ok(());
    }
    if case == 10 {
        ensure!(
            c.state()?
                .prompts
                .iter()
                .any(|prompt| prompt == &data(profile))
        );
        return Ok(());
    }
    c.stop()?;
    if case == 11 {
        ensure!(fs::read(&c.path)? == native);
        return Ok(());
    }
    if case == 12 {
        let pending =
            c.f.repo
                .join(".trailvault/metadata")
                .join(&c.id)
                .join("full.jsonl");
        ensure!(pending.is_file() && fs::metadata(pending)?.permissions().mode() & 0o777 == 0o600);
        return Ok(());
    }
    if case == 13 {
        ensure!(
            c.state()?
                .files_touched
                .contains(&"cloud change.txt".to_owned())
        );
        return Ok(());
    }
    if (14..=16).contains(&case) {
        let before = c.state()?.step_count;
        c.f.hook(
            ["pre-tool-use", "post-tool-use", "error-occurred"][case - 14],
            &c.payload,
        )?;
        ensure!(c.state()?.step_count == before);
        return Ok(());
    }
    if case == 17 {
        c.f.hook("session-end", &c.payload)?;
        ensure!(c.state()?.phase == "ended");
        return Ok(());
    }
    let (reference, sha) = c.commit()?;
    match case {
        18 => ensure!(
            c.f.g(&["log", "-1", "--format=%B"])?
                .contains("Trailvault-Checkpoint:")
        ),
        19 => ensure!(c.f.g(&["show", "-s", "--format=%P", &sha])?.is_empty()),
        20 => {
            let meta: Value = serde_json::from_str(&c.artifact(&sha, "metadata.json")?)?;
            ensure!(meta["sessions"][0]["transcript"] == "/0/full.jsonl");
        }
        21 => {
            let parsed = redact::parse(&c.artifact(&sha, "0/full.jsonl")?)?;
            ensure!(parsed.len() == c.events.len() && parsed[0]["data"]["sessionId"] == c.id);
        }
        22 => {
            let parsed = redact::parse(&c.artifact(&sha, "0/transcript.jsonl")?)?;
            ensure!(
                parsed[0]["type"] == "header" && parsed.iter().any(|row| row["role"] == "user")
            );
        }
        23 => ensure!(
            redact::parse(&c.artifact(&sha, "0/full.jsonl")?)?
                .iter()
                .any(|row| row["type"] == "tool.execution_start"
                    && row["data"]["toolCallId"] == "call-one")
        ),
        24 => ensure!(
            redact::parse(&c.artifact(&sha, "0/full.jsonl")?)?
                .iter()
                .any(|row| row["type"] == "tool.execution_complete"
                    && row["data"]["success"] == !matches!(profile, 10 | 11))
        ),
        25 => ensure!(
            c.artifact(&sha, "0/content_hash.txt")?
                == format!(
                    "sha256:{}\n",
                    capture::hash(c.artifact(&sha, "0/full.jsonl")?)
                )
        ),
        26 => {
            let full = c.artifact(&sha, "0/full.jsonl")?;
            ensure!(!full.contains("synthetic-cloud-fixture-72849"));
            if profile == 19 {
                ensure!(full.contains("REDACTED"));
            }
        }
        27 => ensure!(
            !git::run(
                &c.f.repo,
                &["merge-base", "--is-ancestor", &sha, "HEAD"],
                &c.f.env,
                None
            )?
            .success
        ),
        28 => {
            let queue: Vec<String> = git::read(&c.f.repo.join(".git/trailvault/push-queue.json"))?;
            ensure!(queue.contains(&reference));
        }
        29..=31 => {
            let remote = c.f.remote()?;
            c.f.g(&["push", "origin", "main"])?;
            let remote_sha = support::g(
                c.f.temp.path(),
                &[
                    "--git-dir",
                    support::text(&remote).as_str(),
                    "rev-parse",
                    &reference,
                ],
            )?;
            ensure!(remote_sha == sha);
            if case == 30 {
                let queue: Vec<String> =
                    git::read(&c.f.repo.join(".git/trailvault/push-queue.json"))?;
                ensure!(queue.is_empty());
            }
            if case == 31 {
                let fresh = c.f.temp.path().join("fresh");
                support::g(
                    c.f.temp.path(),
                    &[
                        "clone",
                        "--branch",
                        "main",
                        support::text(&remote).as_str(),
                        support::text(&fresh).as_str(),
                    ],
                )?;
                support::g(
                    &fresh,
                    &["fetch", "origin", &format!("{reference}:{reference}")],
                )?;
                ensure!(support::g(&fresh, &["rev-parse", &reference])? == sha);
            }
        }
        32 => ensure!(
            !c.f.g(&["branch", "--list"])?
                .contains("trailvault/checkpoints")
                && c.f.g(&["show-ref"])?.contains(&reference)
        ),
        _ => unreachable!(),
    }
    Ok(())
}

fn main() {
    let args = Arguments::from_args();
    assert_eq!(CASES.len() * PROFILES.len(), 1000);
    eprintln!("OFFLINE cloud-shaped contract tests; these are not 1000 hosted Copilot sessions.");
    let records = Arc::new(Mutex::new(Vec::new()));
    let trials = CASES
        .iter()
        .enumerate()
        .flat_map(|(case, name)| {
            let records = Arc::clone(&records);
            PROFILES.iter().enumerate().map(move |(profile, payload)| {
                let records = Arc::clone(&records);
                Trial::test(format!("cloud::{name}::{payload}"), move || {
                    let result = run(case, profile);
                    records.lock().expect("cloud results mutex").push(json!({
                        "scenario":name,"profile":payload,"passed":result.is_ok(),
                        "error":result.as_ref().err().map(|error|format!("{error:#}"))
                    }));
                    result.map_err(|error| format!("{error:#}").into())
                })
            })
        })
        .collect();
    let conclusion = libtest_mimic::run(&args, trials);
    let records = records.lock().expect("cloud results mutex");
    git::save(&support::project().join(".test-results/copilot-cloud-offline.json"),&json!({
        "timestamp":chrono::Utc::now().to_rfc3339(),"execution":"local-offline-cloud-shaped-contracts",
        "hosted_sessions":0,"scenario_count":CASES.len(),"profile_count":PROFILES.len(),
        "tests_executed":records.len(),"passed":records.iter().filter(|record|record["passed"]==true).count(),
        "results":*records
    })).expect("Cannot persist offline cloud contract evidence");
    conclusion.exit();
}
