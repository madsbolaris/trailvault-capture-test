mod support;
use anyhow::Result;
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs};
use support::*;
use trailvault::{
    capture, git,
    redact::{self, Redactor},
};

fn trials() -> Vec<Trial> {
    let mut trials = Vec::new();
    macro_rules! case {
        ($name:expr, $body:expr) => {
            trials.push(Trial::test(
                $name,
                || -> std::result::Result<(), libtest_mimic::Failed> {
                    let run = || -> Result<()> { $body };
                    let result = run();
                    result.map_err(|e| format!("{e:#}").into())
                },
            ));
        };
    }
    for (name, value) in [
        (
            "github provider token",
            format!("ghp_{}", "aBcDeF0123456789".repeat(3)),
        ),
        ("AWS identifier", "AKIAZ7M4Q2N8R6T3V5WX".into()),
        (
            "OpenAI provider token",
            format!("sk-proj-{}", "x9K4nY2z".repeat(6)),
        ),
        ("password assignment", "DB_PASSWORD=purple-lantern".into()),
        ("API key assignment", "DEMO_API_KEY=violet-otter".into()),
        (
            "credential URL",
            "postgres://fakeuser:fakepassword@db.example.invalid/demo".into(),
        ),
        (
            "JDBC password",
            "jdbc:postgresql://db.example.invalid/demo?user=demo&password=synthetic-secret".into(),
        ),
    ] {
        trials.push(Trial::test(
            format!("redaction removes {name}"),
            move || {
                assert_ne!(Redactor::new(&json!({})).unwrap().text(&value), value);
                Ok(())
            },
        ));
    }
    for value in [
        "the weather is pleasant",
        "Version=10.1.2.3",
        "API_KEY=your-api-key-here",
        "This fucking deployment is shit.",
        "/Users/fakealex/Clients/private/payroll.csv",
    ] {
        trials.push(Trial::test(
            format!("default retains explicit control: {value}"),
            move || {
                assert_eq!(Redactor::new(&json!({})).unwrap().text(value), value);
                Ok(())
            },
        ));
    }
    case!("PII is opt-in", {
        assert_eq!(
            Redactor::new(&json!({}))?.text("alex@example.invalid"),
            "alex@example.invalid"
        );
        Ok(())
    });
    case!("PII email enabled", {
        assert_eq!(
            Redactor::new(&json!({"redaction":{"pii":{"enabled":true}}}))?
                .text("alex@example.invalid"),
            "[REDACTED_EMAIL]"
        );
        Ok(())
    });
    case!("PII preserves invalid IPv4 literal", {
        assert_eq!(
            Redactor::new(&json!({"redaction":{"pii":{"enabled":true}}}))?.text("999.999.999.999"),
            "999.999.999.999"
        );
        Ok(())
    });
    case!(
        "custom rules redact nested text but not structural paths",
        {
            let mut r = Redactor::new(
                &json!({"redaction":{"custom_redactions":{"project":"PRIVATE-LAB"}}}),
            )?;
            let e = r.event(&json!({"type":"user.message","data":{"content":"PRIVATE-LAB","context":{"cwd":"/PRIVATE-LAB"},"arguments":{"content":"PRIVATE-LAB","path":"/PRIVATE-LAB"}}}));
            assert_eq!(e["type"], "user.message");
            assert_eq!(e["data"]["content"], "REDACTED");
            assert_eq!(e["data"]["context"]["cwd"], "/PRIVATE-LAB");
            assert_eq!(e["data"]["arguments"]["content"], "REDACTED");
            assert_eq!(e["data"]["arguments"]["path"], "/PRIVATE-LAB");
            Ok(())
        }
    );
    case!(
        "real Copilot transformed prompts and hook tool-result copies are redacted",
        {
            let mut r = Redactor::new(
                &json!({"redaction":{"custom_redactions":{"project":"PRIVATE-LAB-[0-9]+"}}}),
            )?;
            let events = [
                json!({"type":"user.message","data":{"content":"DEMO_API_KEY=violet-otter","transformedContent":"DEMO_API_KEY=violet-otter PRIVATE-LAB-123456"}}),
                json!({"type":"hook.start","data":{"input":{"toolResult":{"textResultForLlm":"DEMO_API_KEY=violet-otter PRIVATE-LAB-123456","sessionLog":"DEMO_API_KEY=violet-otter PRIVATE-LAB-123456"}}}}),
            ];
            let stored =
                serde_json::to_string(&events.iter().map(|e| r.event(e)).collect::<Vec<_>>())?;
            assert!(!stored.contains("violet-otter"));
            assert!(!stored.contains("PRIVATE-LAB-123456"));
            Ok(())
        }
    );
    for (name, config) in [
        (
            "RE2 rejects backreferences",
            json!({"custom_redactions":{"bad":"(x)\\1"}}),
        ),
        (
            "empty-match regex fails explicitly",
            json!({"custom_redactions":{"bad":"a*"}}),
        ),
        (
            "invalid regex fails explicitly",
            json!({"custom_redactions":{"bad":"["}}),
        ),
        (
            "non-string regex fails explicitly",
            json!({"custom_redactions":{"bad":123}}),
        ),
        (
            "non-object regex map fails explicitly",
            json!({"custom_redactions":"secret"}),
        ),
        (
            "non-boolean PII toggle fails explicitly",
            json!({"pii":{"enabled":"false"}}),
        ),
    ] {
        trials.push(Trial::test(name, move || {
            assert!(Redactor::new(&json!({"redaction":config})).is_err());
            Ok(())
        }));
    }
    case!(
        "transcript parser rejects malformed line with line number",
        {
            assert!(
                format!(
                    "{:#}",
                    redact::parse("{\"type\":\"ok\"}\nnope\n").unwrap_err()
                )
                .contains("line 2")
            );
            Ok(())
        }
    );
    case!("transcript parser rejects event without type", {
        assert!(redact::parse("{}\n").is_err());
        Ok(())
    });
    case!("compact representation keeps user and assistant roles", {
        let rows = redact::parse(&redact::compact(&[
            event("user.message", json!({"content":"Hello"})),
            event("assistant.message", json!({"content":"Hi"})),
        ])?)?;
        assert_eq!(rows[0]["version"], 2);
        assert_eq!(rows[0]["classification_version"], 1);
        assert_eq!(rows[1]["role"], "user");
        assert_eq!(rows[2]["role"], "assistant");
        assert_eq!(rows[1]["classification"]["provenance"], "user_prompts");
        assert_eq!(
            rows[2]["classification"]["provenance"],
            "assistant_responses"
        );
        assert_eq!(rows[1]["classification"]["activity"], "unknown");
        assert_eq!(rows[2]["classification"]["activity"], "unknown");
        Ok(())
    });
    case!("1000 generated checkpoint IDs are unique", {
        let ids: BTreeSet<_> = (0..1000).map(|_| capture::checkpoint_id()).collect();
        assert_eq!(ids.len(), 1000);
        for id in ids {
            capture::checkpoint_ref(&id)?;
        }
        Ok(())
    });
    for id in [
        "../main",
        "--all",
        "",
        "aaaaaaaaaaaaaaaaaaaaaaaaaa",
        "undefined",
    ] {
        trials.push(Trial::test(
            format!("invalid checkpoint ID rejected: {id}"),
            move || {
                assert!(capture::checkpoint_ref(id).is_err());
                Ok(())
            },
        ));
    }
    case!("configuration merges shared and local privacy settings", {
        let f = Fixture::new()?;
        git::save(
            &f.repo.join(".trailvault/settings.json"),
            &json!({"enabled":true,"redaction":{"pii":{"enabled":true}}}),
        )?;
        f.configure(&json!({"redaction":{"custom_redactions":{"project":"PRIVATE-LAB"}}}))?;
        let c = capture::settings(&git::Repo::discover(&f.repo)?)?;
        assert_eq!(c["redaction"]["pii"]["enabled"], true);
        assert_eq!(
            c["redaction"]["custom_redactions"]["project"],
            "PRIVATE-LAB"
        );
        Ok(())
    });
    for (name, config) in [
        (
            "invalid commit linking preference fails explicitly",
            json!({"commit_linking":"maybe"}),
        ),
        (
            "invalid push flag fails explicitly",
            json!({"strategy_options":{"push_sessions":"false"}}),
        ),
        (
            "prototype pollution settings rejected",
            json!({"__proto__":{"enabled":false}}),
        ),
    ] {
        trials.push(Trial::test(name, move || {
            let f = Fixture::new().map_err(|e| format!("{e:#}"))?;
            f.configure(&config).map_err(|e| format!("{e:#}"))?;
            assert!(f.cli(&["status"]).is_err());
            Ok(())
        }));
    }
    case!("malformed local settings fail explicitly", {
        let f = Fixture::new()?;
        put(&f.repo.join(".trailvault/settings.local.json"), "{")?;
        assert!(f.cli(&["status"]).is_err());
        Ok(())
    });
    case!("disabled capture writes no new session state", {
        let f = Fixture::new()?;
        f.cli(&["disable"])?;
        f.capture("agent.txt")?;
        assert!(!f.repo.join(".git/trailvault-sessions").exists());
        assert_eq!(f.status()?["enabled"], false);
        Ok(())
    });
    case!(
        "enable reverses local disable without losing custom settings",
        {
            let f = Fixture::new()?;
            f.configure(&json!({"redaction":{"custom_redactions":{"project":"PRIVATE-LAB"}}}))?;
            f.cli(&["disable"])?;
            f.cli(&["enable"])?;
            assert_eq!(f.status()?["enabled"], true);
            assert_eq!(
                capture::settings(&git::Repo::discover(&f.repo)?)?["redaction"]["custom_redactions"]
                    ["project"],
                "PRIVATE-LAB"
            );
            Ok(())
        }
    );
    case!(
        "disabled capture leaves unrelated pre-existing hooks active",
        {
            let f = Fixture::bare()?;
            script(
                &f.repo.join(".git/hooks/post-commit"),
                "#!/bin/sh\ntouch \"$(git rev-parse --git-dir)/hook-ran\"\n",
            )?;
            f.enable()?;
            fs::remove_file(f.repo.join(".git/hook-ran"))?;
            f.cli(&["disable"])?;
            put(&f.repo.join("human.txt"), "human\n")?;
            f.g(&["add", "human.txt"])?;
            f.g(&["commit", "-m", "Human"])?;
            assert!(f.repo.join(".git/hook-ran").exists());
            Ok(())
        }
    );
    case!(
        "existing hook is backed up exactly once on repeated enable",
        {
            let f = Fixture::bare()?;
            script(
                &f.repo.join(".git/hooks/post-commit"),
                "#!/bin/sh\nexit 0\n",
            )?;
            f.enable()?;
            let path = f.repo.join(".git/hooks/post-commit.pre-trailvault");
            let before = fs::read(&path)?;
            f.cli(&["enable"])?;
            assert_eq!(fs::read(&path)?, before);
            Ok(())
        }
    );
    case!(
        "enable refuses to modify a shared external hooks directory",
        {
            let f = Fixture::new()?;
            let shared = f.temp.path().join("shared-hooks");
            fs::create_dir(&shared)?;
            f.g(&["config", "core.hooksPath", &text(&shared)])?;
            assert!(f.cli(&["enable"]).is_err());
            assert_eq!(fs::read_dir(shared)?.count(), 0);
            Ok(())
        }
    );
    for (name, hook, payload) in [
        (
            "session ID traversal is rejected",
            "session-start",
            json!({"sessionId":"../escape"}),
        ),
        (
            "missing session ID fails explicitly",
            "session-start",
            json!({}),
        ),
        ("unknown hook fails explicitly", "bogus", json!({})),
    ] {
        trials.push(Trial::test(name, move || {
            let f = Fixture::new().map_err(|e| format!("{e:#}"))?;
            assert!(f.hook(hook, &payload).is_err());
            Ok(())
        }));
    }
    case!("truncated transcript is not captured successfully", {
        let f = Fixture::new()?;
        let s = f.capture("agent.txt")?;
        put(&s.transcript, "{\"type\":")?;
        assert!(f.hook("agent-stop", &s.payload).is_err());
        Ok(())
    });
    case!("transcript outside agent storage is rejected", {
        let f = Fixture::new()?;
        let mut s = f.capture("agent.txt")?;
        let outside = f.temp.path().join("agent/not-a-session.jsonl");
        put(
            &outside,
            "{\"type\":\"user.message\",\"data\":{\"content\":\"fixture\"}}\n",
        )?;
        s.payload["transcriptPath"] = json!(outside);
        assert!(f.hook("agent-stop", &s.payload).is_err());
        Ok(())
    });
    case!("multiple sessions retain distinct pending state", {
        let f = Fixture::new()?;
        let first = f.capture("first.txt")?;
        let second = f.capture("second.txt")?;
        f.g(&["add", "first.txt", "second.txt"])?;
        f.g(&["commit", "-m", "Two agent sessions"])?;
        let id = capture::trailer(&f.g(&["log", "-1", "--format=%B"])?, trailer_name()).unwrap();
        let reference = capture::checkpoint_ref(&id)?;
        let root: Value =
            serde_json::from_str(&f.g(&["show", &format!("{reference}:metadata.json")])?)?;
        assert_eq!(root["sessions"].as_array().unwrap().len(), 2);
        let stored: BTreeSet<String> = (0..2)
            .map(|n| -> Result<String> {
                let v: Value = serde_json::from_str(
                    &f.g(&["show", &format!("{reference}:{n}/metadata.json")])?,
                )?;
                Ok(v["session_id"].as_str().unwrap().into())
            })
            .collect::<Result<_>>()?;
        assert_eq!(stored, BTreeSet::from([first.id, second.id]));
        Ok(())
    });
    case!(
        "removing editor trailer declines capture and preserves pending state",
        {
            let mut f = Fixture::new()?;
            f.capture("agent.txt")?;
            f.g(&["add", "agent.txt"])?;
            let message = f.temp.path().join("message.txt");
            put(&message, "Editor commit\n\n# Comment\n")?;
            f.cli(&["hooks", "git", "prepare-commit-msg", &text(&message)])?;
            assert!(
                fs::read_to_string(message)?.contains("Remove the Trailvault-Checkpoint trailer")
            );
            let editor = f.temp.path().join("decline.sh");
            script(
                &editor,
                "#!/bin/sh\nprintf 'Declined context\\n' > \"$1\"\n",
            )?;
            f.env.insert("GIT_EDITOR".into(), text(&editor));
            f.g(&["commit"])?;
            assert!(
                !f.g(&["log", "-1", "--format=%B"])?
                    .contains("Trailvault-Checkpoint:")
            );
            assert!(
                f.g(&[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/trailvault/checkpoints"
                ])?
                .is_empty()
            );
            assert_eq!(
                f.status()?["sessions"][0]["files_touched"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            Ok(())
        }
    );
    for answer in ["y", "n", "a"] {
        trials.push(Trial::test(
            format!("real terminal confirmation accepts {answer}"),
            move || terminal(answer).map_err(|e| format!("{e:#}").into()),
        ));
    }
    case!(
        "failed automatic context push keeps queue but does not block code push",
        {
            let f = Fixture::new()?;
            f.remote()?;
            let (_, reference) = f.commit_capture()?;
            f.configure(&json!({"checkpoint_push_remote":f.temp.path().join("missing.git")}))?;
            f.g(&["push", "-u", "origin", "main"])?;
            assert!(
                f.status()?["queued"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(reference))
            );
            Ok(())
        }
    );
    case!("failed explicit upload exits nonzero and keeps queue", {
        let f = Fixture::new()?;
        f.commit_capture()?;
        assert!(
            f.cli(&["push", &text(&f.temp.path().join("missing.git"))])
                .is_err()
        );
        assert_eq!(f.status()?["queued"].as_array().unwrap().len(), 1);
        Ok(())
    });
    case!("retry succeeds and clears durable upload queue", {
        let f = Fixture::new()?;
        let destination = f.remote()?;
        let (_, reference) = f.commit_capture()?;
        assert!(
            f.cli(&["push", &text(&f.temp.path().join("missing.git"))])
                .is_err()
        );
        f.cli(&["push", "origin"])?;
        assert_eq!(f.status()?["queued"], json!([]));
        assert_eq!(
            g(
                f.temp.path(),
                &[
                    &format!("--git-dir={}", destination.display()),
                    "rev-parse",
                    &reference
                ]
            )?,
            f.g(&["rev-parse", &reference])?
        );
        Ok(())
    });
    case!("automatic uploads can be paused while source pushes", {
        let f = Fixture::new()?;
        let destination = f.remote()?;
        f.commit_capture()?;
        f.configure(&json!({"strategy_options":{"push_sessions":false}}))?;
        f.g(&["push", "-u", "origin", "main"])?;
        assert_eq!(f.status()?["queued"].as_array().unwrap().len(), 1);
        assert!(
            g(
                f.temp.path(),
                &[
                    &format!("--git-dir={}", destination.display()),
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/trailvault/checkpoints"
                ]
            )?
            .is_empty()
        );
        Ok(())
    });
    case!("fetch command retrieves checkpoint into a new clone", {
        let f = Fixture::new()?;
        let destination = f.remote()?;
        let (_, reference) = f.commit_capture()?;
        f.g(&["push", "-u", "origin", "main"])?;
        let clone = f.temp.path().join("clone");
        g(
            f.temp.path(),
            &["clone", "-b", "main", &text(&destination), &text(&clone)],
        )?;
        invoke(&clone, &["fetch"], &f.env, None)?;
        assert_eq!(
            g(&clone, &["rev-parse", &reference])?,
            f.g(&["rev-parse", &reference])?
        );
        Ok(())
    });
    case!(
        "no pending context means push performs no context upload",
        {
            let f = Fixture::new()?;
            f.cli(&["push", &text(&f.temp.path().join("missing.git"))])?;
            assert_eq!(f.status()?["queued"], json!([]));
            Ok(())
        }
    );
    case!("an aborted commit does not create a checkpoint", {
        let f = Fixture::new()?;
        f.capture("agent.txt")?;
        f.g(&["add", "agent.txt"])?;
        script(&f.repo.join(".git/hooks/commit-msg"), "#!/bin/sh\nexit 1\n")?;
        assert!(f.g(&["commit", "-m", "Rejected"]).is_err());
        assert!(
            f.g(&[
                "for-each-ref",
                "--format=%(refname)",
                "refs/trailvault/checkpoints"
            ])?
            .is_empty()
        );
        assert_eq!(
            f.status()?["sessions"][0]["files_touched"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        Ok(())
    });
    case!(
        "recover finishes a checkpoint after post-commit hook failure",
        {
            let f = Fixture::new()?;
            f.capture("agent.txt")?;
            f.g(&["add", "agent.txt"])?;
            let hook = f.repo.join(".git/hooks/post-commit");
            let original = fs::read(&hook)?;
            script(&hook, "#!/bin/sh\nexit 1\n")?;
            f.g(&["commit", "-m", "Interrupted capture"])?;
            assert!(
                f.g(&[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/trailvault/checkpoints"
                ])?
                .is_empty()
            );
            script(&hook, std::str::from_utf8(&original)?)?;
            f.cli(&["recover"])?;
            assert_eq!(f.status()?["queued"].as_array().unwrap().len(), 1);
            Ok(())
        }
    );
    case!(
        "amend without new agent work retains original checkpoint link",
        {
            let f = Fixture::new()?;
            let (_, reference) = f.commit_capture()?;
            f.g(&["commit", "--amend", "--no-edit"])?;
            assert!(
                f.g(&["log", "-1", "--format=%B"])?
                    .contains(reference.rsplit('/').next().unwrap())
            );
            f.g(&["rev-parse", &reference])?;
            Ok(())
        }
    );
    case!(
        "linked worktree stores sessions in shared Git directory without cross-linking",
        {
            let mut f = Fixture::new()?;
            let original = f.repo.clone();
            let sibling = f.temp.path().join("worktree");
            f.g(&["worktree", "add", "-b", "feature", &text(&sibling)])?;
            f.repo = sibling;
            f.cli(&["enable"])?;
            let (_, reference) = f.commit_capture()?;
            assert_eq!(f.status()?["sessions"].as_array().unwrap().len(), 1);
            f.repo = original;
            assert_eq!(f.status()?["sessions"], json!([]));
            f.g(&["rev-parse", &reference])?;
            Ok(())
        }
    );
    case!("repository lock cleans up after callback error", {
        let f = Fixture::new()?;
        let repo = git::Repo::discover(&f.repo)?;
        let failed = (|| -> Result<()> {
            let _guard = repo.lock()?;
            anyhow::bail!("test failure")
        })();
        assert!(failed.is_err());
        let _guard = repo.lock()?;
        Ok(())
    });
    case!("atomic writes replace whole files", {
        let f = Fixture::new()?;
        let path = f.temp.path().join("atomic.txt");
        git::atomic(&path, b"first")?;
        git::atomic(&path, b"second")?;
        assert_eq!(fs::read_to_string(path)?, "second");
        Ok(())
    });
    case!("unknown commands return explicit errors", {
        let f = Fixture::new()?;
        assert!(f.cli(&["bogus"]).is_err());
        Ok(())
    });
    case!("remote option injection rejected", {
        let f = Fixture::new()?;
        assert!(f.cli(&["fetch", "--upload-pack=bad"]).is_err());
        Ok(())
    });
    trials
}
fn terminal(answer: &str) -> Result<()> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::{
        io::{Read, Write},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    let f = Fixture::new()?;
    f.capture("agent.txt")?;
    f.configure(&json!({"commit_linking":"prompt"}))?;
    f.g(&["add", "agent.txt"])?;
    let message = f.temp.path().join("message.txt");
    put(&message, "Interactive fixture\n")?;
    let pair = native_pty_system().openpty(PtySize::default())?;
    let mut cmd = CommandBuilder::new(executable()?);
    cmd.args([
        "hooks",
        "git",
        "prepare-commit-msg",
        &text(&message),
        "message",
    ]);
    cmd.cwd(&f.repo);
    cmd.env("COPILOT_AGENT_SESSION_ID", "");
    for (key, value) in &f.env {
        cmd.env(key, value);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let (tx, rx) = mpsc::channel();
    let read_thread = thread::spawn(move || {
        let mut buffer = [0; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    if error.raw_os_error() != Some(nix::libc::EIO) {
                        let _ = tx.send(format!("Read error: {error}").into_bytes());
                    }
                    break;
                }
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = String::new();
    let mut answered = false;
    let result = (|| -> Result<()> {
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(bytes) => {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                    if !answered && output.contains("[a]lways:") {
                        writer.write_all(format!("{answer}\n").as_bytes())?;
                        answered = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    anyhow::bail!("Terminal timed out: {output}")
                }
            }
        }
        anyhow::ensure!(answered, "CLI did not prompt: {output}");
        anyhow::ensure!(child.wait()?.success(), "CLI failed: {output}");
        assert_eq!(
            fs::read_to_string(&message)?.contains("Trailvault-Checkpoint:"),
            answer != "n"
        );
        if answer == "a" {
            assert_eq!(
                capture::settings(&git::Repo::discover(&f.repo)?)?["commit_linking"],
                "always"
            );
        }
        Ok(())
    })();
    if result.is_err() {
        child.kill()?;
        child.wait()?;
    }
    drop(writer);
    drop(pair.master);
    read_thread.join().expect("PTY reader panicked");
    result
}
fn main() {
    let mut args = Arguments::from_args();
    args.test_threads = Some(1);
    let tests = trials();
    assert_eq!(tests.len(), 66, "Preserve all 66 supplementary tests");
    let conclusion = libtest_mimic::run(&args, tests);
    report("supplementary.json",&json!({"implementation":"rust","tests":conclusion.num_passed+conclusion.num_failed,"passed":conclusion.num_passed,"failures":conclusion.num_failed,"ignored":conclusion.num_ignored,"filtered":conclusion.num_filtered_out})).expect("Cannot persist supplementary evidence");
    conclusion.exit();
}
