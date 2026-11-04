use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use trailvault::{capture, git, redact, restore};

const CASES: &[&str] = &[
    "conversation-content",
    "new-session-identity",
    "working-directory-rebound",
    "source-context-removed",
    "timestamps-retained",
    "event-identity-retained",
    "parent-chain-retained",
    "tool-arguments-retained",
    "tool-results-retained",
    "parallel-tools-retained",
    "failed-tool-retained",
    "resumed-history-retained",
    "system-message-retained",
    "usage-retained",
    "model-history-retained",
    "diagnostics-retained",
    "unknown-event-explicit-loss",
    "child-explicit-loss",
    "redaction-requires-approval",
    "redaction-approved",
    "pending-tool-requires-approval",
    "pending-tool-approved",
    "compaction-rejected",
    "attachment-requires-approval",
    "attachment-approved",
    "compact-input-rejected",
    "empty-input-rejected",
    "malformed-json-rejected",
    "nonobject-event-rejected",
    "missing-header-rejected",
    "duplicate-header-rejected",
    "wrong-native-version-rejected",
    "wrong-producer-rejected",
    "session-identity-mismatch-rejected",
    "duplicate-event-id-rejected",
    "invalid-timestamp-rejected",
    "broken-parent-rejected",
    "missing-content-rejected",
    "duplicate-tool-start-rejected",
    "orphan-tool-result-rejected",
    "hash-mismatch-rejected",
    "wrong-agent-rejected",
    "checkpoint-mismatch-rejected",
    "missing-workdir-rejected",
    "private-atomic-files",
    "existing-session-preserved",
    "symlink-home-rejected",
    "dry-preparation-writes-nothing",
    "provenance-retained",
    "source-byte-preservation",
];
const PROFILES: &[&str] = &[
    "ascii",
    "unicode",
    "emoji",
    "multiline",
    "quotes",
    "backslashes",
    "markdown",
    "html-inert",
    "shell-inert",
    "redacted-marker",
    "long-text",
    "tabs",
    "crlf-records",
    "blank-records",
    "no-final-newline",
    "json-content",
    "empty-assistant",
    "null-tool-result",
    "structured-tool-output",
    "offset-timestamps",
];
const SOURCE: &str = "10000000-0000-4000-8000-000000000001";
const CHECKPOINT: &str = "01M670FBAQ4R23ZYAA58E8822X";

fn ev(id: &str, parent: Option<&str>, kind: &str, mut data: Value, time: &str) -> Value {
    let identity = |name: &str| {
        let hash = capture::hash(name);
        format!(
            "{}-{}-{}-{}-{}",
            &hash[..8],
            &hash[8..12],
            &hash[12..16],
            &hash[16..20],
            &hash[20..32]
        )
    };
    if kind == "assistant.message" {
        data["messageId"] = json!(identity(id));
    }
    json!({"id":identity(id),"parentId":parent.map(identity),"type":kind,"timestamp":time,"data":data})
}
fn fixture(profile: usize) -> Result<(restore::Snapshot, restore::Options, tempfile::TempDir)> {
    let temp = tempfile::tempdir()?;
    let work = temp.path().join(format!("working directory {profile}"));
    fs::create_dir(&work)?;
    let text = match profile {
        1 => "caf\u{e9} \u{65e5}\u{672c}",
        2 => "\u{1f331}\u{1f9ed}",
        3 => "first\nsecond\nthird",
        4 => "\"quoted\" 'single'",
        5 => r"C:\fictional\route\file",
        6 => "## Heading\n```rust\nfn main() {}\n```",
        7 => "<script>throw new Error('inert')</script>",
        8 => "$(touch NEVER_EXECUTE); echo inert",
        9 => "REDACTED [REDACTED_EMAIL]",
        10 => &"long fictional context ".repeat(1024),
        11 => "first\tsecond",
        15 => "{\"type\":\"not-an-event\",\"nested\":[1,true,null]}",
        _ => "Remember the fictional route marker fern-lantern.",
    };
    let timestamp = if profile == 19 {
        "2026-10-30T20:16:43.852+02:00"
    } else {
        "2026-10-30T18:16:43.852Z"
    };
    let events = vec![
        ev(
            "start",
            None,
            "session.start",
            json!({"sessionId":SOURCE,"startTime":timestamp,"version":1,"producer":"copilot-agent","copilotVersion":"1.0.105","context":{"cwd":"/fictional/original","gitRoot":"/fictional/original","branch":"old","headCommit":"old-sha","repository":"fictional/original"}}),
            timestamp,
        ),
        ev(
            "user",
            Some("start"),
            "user.message",
            json!({"content":text}),
            timestamp,
        ),
        ev(
            "assistant",
            Some("user"),
            "assistant.message",
            json!({"content":if profile==16 {""} else {"Acknowledged."},"toolRequests":[{"toolCallId":"tool-1","name":"shell","arguments":{"command":"echo inert"}}]}),
            timestamp,
        ),
        ev(
            "tool-start",
            Some("assistant"),
            "tool.execution_start",
            json!({"toolCallId":"tool-1","toolName":"shell","arguments":{"command":text,"nested":{"number":profile,"array":[true,null]}}}),
            timestamp,
        ),
        ev(
            "tool-result",
            Some("tool-start"),
            "tool.execution_complete",
            json!({"toolCallId":"tool-1","success":true,"result":if profile==17 {Value::Null} else if profile==18 {json!({"content":text,"detailedContent":[{"type":"text","text":text}]})} else {json!({"content":text})}}),
            timestamp,
        ),
    ];
    let mut full = redact::jsonl(&events)?;
    if profile == 12 {
        full = full.replace('\n', "\r\n");
    }
    if profile == 13 {
        full = format!("\n\n{}\n\n", full.replace('\n', "\n\n"));
    }
    if profile == 14 {
        full = full.trim_end().into();
    }
    let snapshot = restore::Snapshot {
        checkpoint_id: CHECKPOINT.into(),
        checkpoint_commit: "7d9bc9eba8892569dded559214cd2b7fb838f001".into(),
        metadata: json!({"checkpoint_id":CHECKPOINT,"session_id":SOURCE,"agent":"Copilot CLI","redaction":{"replacements":0}}),
        content_hash: format!("sha256:{}", capture::hash(&full)),
        full,
        tasks: vec![],
    };
    let options = restore::Options {
        working_directory: work,
        allow_lossy: false,
    };
    Ok((snapshot, options, temp))
}
fn run(case: usize, profile: usize) -> Result<()> {
    let (mut snapshot, mut options, temp) = fixture(profile)?;
    let mut events = redact::parse(&snapshot.full)?;
    let time = "2026-10-30T18:16:43.852Z";
    let mut expected_error = None;
    let mut expected_warning = None;
    let mut check_tail = None;
    match case {
        9 => {
            events.insert(4,ev("parallel",Some("tool-start"),"tool.execution_start",json!({"toolCallId":"tool-2","toolName":"view","arguments":{"path":"fictional.txt"}}),time));
            events.push(ev(
                "parallel-end",
                Some("tool-result"),
                "tool.execution_complete",
                json!({"toolCallId":"tool-2","success":true,"result":{"content":"second tool"}}),
                time,
            ));
        }
        10 => {
            events[4]["data"]["success"] = json!(false);
            events[4]["data"]["error"] = json!({"message":"fictional tool failure"});
        }
        11 => {
            events.push(ev(
                "resume",
                Some("tool-result"),
                "session.resume",
                json!({"sessionId":SOURCE}),
                time,
            ));
            events.push(ev(
                "followup",
                Some("resume"),
                "user.message",
                json!({"content":"Continue the route task."}),
                time,
            ));
        }
        12..=15 => {
            let kind = [
                "system.message",
                "session.usage_record",
                "session.model_change",
                "hook.end",
            ][case - 12];
            events.push(ev(
                "extra",
                Some("tool-result"),
                kind,
                json!({"content":"Preserve diagnostic","model":"recorded-model","tokens":42}),
                time,
            ));
            check_tail = Some(kind);
        }
        16 => {
            events.push(ev(
                "future",
                Some("tool-result"),
                "future.native.event",
                json!({"payload":profile}),
                time,
            ));
            options.allow_lossy = true;
            expected_warning = Some("unknown_event");
        }
        17 => {
            snapshot.tasks.push(json!({"agent_id":"20000000-0000-4000-8000-000000000002","tool_use_id":"tool-1","transcript_unavailable_reason":"not captured"}));
            options.allow_lossy = true;
            expected_warning = Some("child_state_unavailable");
        }
        18 | 19 => {
            snapshot.metadata["redaction"]["replacements"] = json!(profile + 1);
            options.allow_lossy = case == 19;
            if case == 18 {
                expected_error = Some("allow-lossy");
            } else {
                expected_warning = Some("redacted_content");
            }
        }
        20 | 21 => {
            events.pop();
            options.allow_lossy = case == 21;
            if case == 20 {
                expected_error = Some("allow-lossy");
            } else {
                expected_warning = Some("incomplete_tools");
            }
        }
        22 => {
            events.push(ev(
                "compact",
                Some("tool-result"),
                "session.compaction_complete",
                json!({"summaryContent":"lost prior context"}),
                time,
            ));
            expected_error = Some("compaction");
        }
        23 | 24 => {
            events[1]["data"]["attachments"] = json!([{"path":"/fictional/missing-image.png"}]);
            options.allow_lossy = case == 24;
            if case == 23 {
                expected_error = Some("allow-lossy");
            } else {
                expected_warning = Some("attachments_unavailable");
            }
        }
        25 => {
            events = vec![json!({"type":"header","version":1,"agent":"Copilot CLI"})];
            expected_error = Some("native");
        }
        26 => {
            events.clear();
            expected_error = Some("empty");
        }
        27 => {
            expected_error = Some("JSON");
        }
        28 => {
            events[2] = json!(["not", "object"]);
            expected_error = Some("object");
        }
        29 => {
            events[0]["type"] = json!("user.message");
            expected_error = Some("session.start");
        }
        30 => {
            events.push(ev(
                "second-start",
                Some("tool-result"),
                "session.start",
                events[0]["data"].clone(),
                time,
            ));
            expected_error = Some("session.start");
        }
        31 => {
            events[0]["data"]["version"] = json!(profile + 2);
            expected_error = Some("version");
        }
        32 => {
            events[0]["data"]["producer"] = json!("another-agent");
            expected_error = Some("producer");
        }
        33 => {
            events[0]["data"]["sessionId"] = json!("20000000-0000-4000-8000-000000000002");
            expected_error = Some("identity");
        }
        34 => {
            events[2]["id"] = events[1]["id"].clone();
            expected_error = Some("duplicate");
        }
        35 => {
            events[2]["timestamp"] = json!("not-a-timestamp");
            expected_error = Some("timestamp");
        }
        36 => {
            events[2]["parentId"] = json!("missing-parent");
            expected_error = Some("parent");
        }
        37 => {
            events[1]["data"]
                .as_object_mut()
                .context("data")?
                .remove("content");
            expected_error = Some("content");
        }
        38 => {
            let mut e = events[3].clone();
            e["id"] = json!(uuid::Uuid::new_v4().to_string());
            e["parentId"] = events[4]["id"].clone();
            events.push(e);
            expected_error = Some("tool");
        }
        39 => {
            events[4]["data"]["toolCallId"] = json!("unknown-tool");
            expected_error = Some("tool");
        }
        40 => {
            expected_error = Some("hash");
        }
        41 => {
            snapshot.metadata["agent"] = json!("Different agent");
            expected_error = Some("agent");
        }
        42 => {
            snapshot.metadata["checkpoint_id"] = json!("01M670QQM0VJN4K0HYX6K1SZ0F");
            expected_error = Some("checkpoint");
        }
        43 => {
            options.working_directory = temp.path().join("does-not-exist");
            expected_error = Some("directory");
        }
        _ => {}
    }
    // Keep byte-layout variants intact when no event mutation is required.
    if matches!(case,9..=17|20..=39) {
        snapshot.full = redact::jsonl(&events)?;
    }
    if case == 27 {
        snapshot.full.push_str("{broken JSON\n");
    }
    snapshot.content_hash = format!("sha256:{}", capture::hash(&snapshot.full));
    if case == 40 {
        snapshot.content_hash = format!("sha256:{}", "0".repeat(64));
    }
    let original = snapshot.full.clone();
    let result = restore::prepare(&snapshot, &options);
    if let Some(expected) = expected_error {
        let error = result
            .err()
            .context("Unsafe restoration unexpectedly accepted")?;
        ensure!(
            format!("{error:#}").contains(expected),
            "Expected {expected:?}; got {error:#}"
        );
        ensure!(
            !temp.path().join("copilot").exists(),
            "Validation failure wrote a destination"
        );
        return Ok(());
    }
    let prepared = result?;
    let new_id = prepared.receipt["restored_session_id"]
        .as_str()
        .context("Missing new ID")?;
    ensure!(
        uuid::Uuid::parse_str(new_id).is_ok() && new_id != SOURCE,
        "Must fork the native identity"
    );
    ensure!(prepared.receipt["source_session_id"] == SOURCE);
    ensure!(snapshot.full == original, "Source snapshot mutated");
    if let Some(warning) = expected_warning {
        ensure!(
            prepared.receipt["warnings"]
                .as_array()
                .context("warnings")?
                .iter()
                .any(|entry| entry["code"] == warning),
            "Missing explicit {warning} warning"
        );
    }
    let restored = redact::parse(&prepared.events)?;
    match case {
        0 => ensure!(restored[1]["data"]["content"] == events[1]["data"]["content"]),
        1 => ensure!(restored[0]["data"]["sessionId"] == new_id),
        2 => ensure!(
            restored[0]["data"]["context"]["cwd"]
                == options
                    .working_directory
                    .canonicalize()?
                    .to_str()
                    .context("path")?
        ),
        3 => ensure!(
            restored[0]["data"]["context"].get("headCommit").is_none()
                && restored[0]["data"]["context"].get("repository").is_none()
        ),
        4 => ensure!(
            restored
                .iter()
                .zip(&events)
                .all(|(a, b)| a["timestamp"] == b["timestamp"])
        ),
        5 => ensure!(
            restored
                .iter()
                .zip(&events)
                .all(|(a, b)| a["id"] == b["id"])
        ),
        6 => ensure!(
            restored
                .iter()
                .zip(&events)
                .all(|(a, b)| a["parentId"] == b["parentId"])
        ),
        7 => ensure!(restored[3]["data"] == events[3]["data"]),
        8 | 10 => ensure!(restored[4]["data"] == events[4]["data"]),
        9 => ensure!(restored.len() == 7 && restored[6]["data"]["toolCallId"] == "tool-2"),
        11 => ensure!(
            restored[5]["data"]["sessionId"] == new_id && restored[6]["data"] == events[6]["data"]
        ),
        12..=15 => ensure!(
            restored.last().context("last")?["type"] == check_tail.context("kind")?
                && restored.last() == events.last()
        ),
        44 => {
            let home = temp.path().join("copilot");
            let path = restore::write(&prepared, &home)?;
            ensure!(path == home.join("session-state").join(new_id));
            ensure!(fs::metadata(&path)?.permissions().mode() & 0o777 == 0o700);
            for name in ["events.jsonl", "workspace.yaml", "trailvault-restore.json"] {
                ensure!(fs::metadata(path.join(name))?.permissions().mode() & 0o777 == 0o600);
            }
            ensure!(fs::read_to_string(path.join("events.jsonl"))? == prepared.events);
            ensure!(git::read::<Value>(&path.join("trailvault-restore.json"))? == prepared.receipt);
        }
        45 => {
            let home = temp.path().join("copilot");
            let path = restore::write(&prepared, &home)?;
            fs::write(path.join("sentinel"), b"existing user content")?;
            ensure!(
                restore::write(&prepared, &home).is_err(),
                "Existing session overwritten"
            );
            ensure!(fs::read(path.join("sentinel"))? == b"existing user content");
        }
        46 => {
            let outside = temp.path().join("outside");
            fs::create_dir(&outside)?;
            let home = temp.path().join("copilot");
            symlink(&outside, &home)?;
            ensure!(
                restore::write(&prepared, &home).is_err(),
                "Symlink destination followed"
            );
            ensure!(fs::read_dir(outside)?.next().is_none());
        }
        47 => ensure!(!temp.path().join("copilot").exists()),
        48 => ensure!(
            prepared.receipt["checkpoint_id"] == CHECKPOINT
                && prepared.receipt["checkpoint_commit"] == snapshot.checkpoint_commit
                && prepared.receipt["source_content_hash"] == snapshot.content_hash
        ),
        49 => ensure!(snapshot.full.as_bytes() == original.as_bytes()),
        _ => {}
    }
    Ok(())
}
fn main() {
    let args = Arguments::from_args();
    assert_eq!(CASES.len() * PROFILES.len(), 1000);
    let trials = CASES
        .iter()
        .enumerate()
        .flat_map(|(case, name)| {
            PROFILES.iter().enumerate().map(move |(profile, payload)| {
                Trial::test(format!("restore::{name}::{payload}"), move || {
                    run(case, profile).map_err(|error| format!("{error:#}").into())
                })
            })
        })
        .collect();
    libtest_mimic::run(&args, trials).exit();
}
