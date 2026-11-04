mod support;
use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};
use support::*;
use trailvault::{capture, git, redact};
use uuid::Uuid;

const CHECKS: &[&str] = &[
    "setup is idempotent",
    "agent identity retained",
    "turn records touched files",
    "pending copy preserves literal",
    "pending transcript has private mode",
    "pending transcript is ignored",
    "turn end creates no checkpoint ref",
    "turn end creates no legacy shadow branch",
    "unrelated staged paths do not link",
    "noninteractive commit links with ULID",
    "source tree is unchanged by hooks",
    "checkpoint is outside source ancestry",
    "first checkpoint commit is orphan",
    "checkpoint tree contains six contract files",
    "root metadata references checkpoint",
    "session metadata references agent session",
    "full transcript is valid JSONL",
    "compact transcript is valid JSONL",
    "raw native transcript remains byte-identical",
    "redaction matches configured literal policy",
    "structured cwd remains unchanged",
    "content digest is present",
    "normal code push publishes checkpoint ref",
    "fresh clone can explicitly fetch checkpoint",
    "partial commit preserves remaining attribution",
];
fn scenario(operation: &str, profile: &str) -> Result<Value> {
    let f = Fixture::bare()?;
    let target = target();
    let remote = f.remote()?;
    f.cli(&[
        "enable",
        "--agent",
        "copilot-cli",
        "--absolute-git-hook-path",
        "--telemetry=false",
    ])?;
    let hooks = f.repo.join(format!(".github/hooks/{target}.json"));
    let first = fs::read(&hooks)?;
    f.cli(&[
        "enable",
        "--agent",
        "copilot-cli",
        "--absolute-git-hook-path",
        "--telemetry=false",
    ])?;
    let idempotent = first == fs::read(&hooks)?;
    let mut settings =
        json!({"enabled":true,"commit_linking":if profile=="plain" {"prompt"} else {"always"}});
    if profile == "custom" {
        settings["redaction"] = json!({"custom_redactions":{"project":"PRIVATE-LAB-[0-9]+"}});
    }
    if profile == "pii" {
        settings["redaction"] = json!({"pii":{"enabled":true}});
    }
    f.configure(&settings)?;
    f.g(&["add", "."])?;
    f.g(&["commit", "-m", "Configure capture"])?;
    let id = Uuid::new_v4().to_string();
    let transcript = PathBuf::from(&f.env["COPILOT_HOME"])
        .join("session-state")
        .join(&id)
        .join("events.jsonl");
    let literal = match profile {
        "plain" => "the weather is pleasant",
        "credential" => "DB_PASSWORD=purple-lantern",
        "custom" => "PRIVATE-LAB-123456",
        "pii" => "alex.privacy@example.invalid",
        _ => "This fucking deployment is shit.",
    };
    let prompt = format!("Synthetic compatibility fixture {operation}/{profile}. {literal}");
    let mut events = vec![
        event(
            "session.start",
            json!({"sessionId":id,"version":1,"producer":"copilot-agent","copilotVersion":"1.0.105","context":{"cwd":f.repo,"gitRoot":f.repo,"branch":"main"}}),
        ),
        event("user.message", json!({"content":prompt})),
    ];
    put(&transcript, redact::jsonl(&events)?)?;
    let payload = json!({"sessionId":id,"cwd":f.repo,"transcriptPath":transcript,"prompt":prompt,"source":"new","initialPrompt":prompt,"timestamp":chrono::Utc::now().timestamp_millis()});
    f.hook("session-start", &payload)?;
    f.hook("user-prompt-submitted", &payload)?;
    let path = match operation {
        "add" => "added.txt",
        "modify" | "delete" => "tracked.txt",
        "nested" => "deep/nested/fixture.txt",
        "spaces" => "folder with spaces/file name.txt",
        "unicode" => "unicode/caf\u{e9}-\u{65e5}\u{672c}.txt",
        "rename" => "new-name.txt",
        _ => "first.txt",
    };
    match operation {
        "delete" => fs::remove_file(f.repo.join(path))?,
        "rename" => fs::rename(f.repo.join("old-name.txt"), f.repo.join(path))?,
        _ => put(&f.repo.join(path), "agent change\n")?,
    }
    if operation == "partial" {
        put(&f.repo.join("second.txt"), "second change\n")?;
    }
    events.push(event(
        "assistant.message",
        json!({"content":format!("Completed fixture. {literal}")}),
    ));
    events.push(event("assistant.turn_end", json!({})));
    put(&transcript, redact::jsonl(&events)?)?;
    let native_hash = capture::hash(fs::read(&transcript)?);
    f.hook("agent-stop", &payload)?;
    let state: Value = git::read(&f.repo.join(format!(".git/{target}-sessions/{id}.json")))?;
    let pending = f.repo.join(format!(".{target}/metadata/{id}/full.jsonl"));
    let local = fs::read_to_string(&pending)?;
    let mode = fs::metadata(&pending)?.permissions().mode() & 0o777;
    let pre_refs = f.g(&[
        "for-each-ref",
        "--format=%(refname)",
        &format!("refs/{target}/checkpoints"),
    ])?;
    let shadows = f.g(&[
        "for-each-ref",
        "--format=%(refname)",
        &format!("refs/heads/{target}/"),
    ])?;
    let ignored = f.g(&["check-ignore", &text(&pending)])?;
    put(&f.repo.join("human-only.txt"), "unrelated human change\n")?;
    f.g(&["add", "human-only.txt"])?;
    f.g(&["commit", "-m", "Unrelated human change"])?;
    let unrelated = f.g(&["log", "-1", "--format=%B"])?;
    f.g(&["add", "--", path])?;
    if operation == "rename" {
        f.g(&["add", "-u"])?;
    }
    let staged = f.g(&["write-tree"])?;
    f.g(&[
        "commit",
        "-m",
        &format!("Fixture {operation}/{profile}"),
        "-m",
        "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>",
    ])?;
    let checkpoint = capture::trailer(&f.g(&["log", "-1", "--format=%B"])?, trailer_name())
        .context("Missing checkpoint")?;
    let reference = format!(
        "refs/{target}/checkpoints/{}/{checkpoint}",
        &checkpoint[24..]
    );
    let show = |path: &str| f.g(&["show", &format!("{reference}:{path}")]);
    let root: Value = serde_json::from_str(&show("metadata.json")?)?;
    let metadata: Value = serde_json::from_str(&show("0/metadata.json")?)?;
    let full = show("0/full.jsonl")?;
    let compact = show("0/transcript.jsonl")?;
    let stored_prompt = show("0/prompt.txt")?;
    let content_hash = show("0/content_hash.txt")?;
    let tree = f.g(&["ls-tree", "-r", "--name-only", &reference])?;
    let source_tree = f.g(&["rev-parse", "HEAD^{tree}"])?;
    let source_parents = f.g(&["rev-list", "--parents", "-1", "HEAD"])?;
    let checkpoint_parents = f.g(&["rev-list", "--parents", "-1", &reference])?;
    let checkpoint_hash = f.g(&["rev-parse", &reference])?;
    f.g(&["push", "-u", "origin", "main"])?;
    let remote_hash = g(
        f.temp.path(),
        &[
            &format!("--git-dir={}", remote.display()),
            "rev-parse",
            &reference,
        ],
    )?;
    let clone = f.temp.path().join("clone");
    g(
        f.temp.path(),
        &["clone", "--quiet", &text(&remote), &text(&clone)],
    )?;
    g(
        &clone,
        &[
            "fetch",
            "origin",
            &format!("+refs/{target}/checkpoints/*:refs/{target}/checkpoints/*"),
        ],
    )?;
    let fetched_hash = g(&clone, &["rev-parse", &reference])?;
    let remaining_linked = if operation == "partial" {
        f.g(&["add", "second.txt"])?;
        f.g(&["commit", "-m", "Remaining agent changes"])?;
        f.g(&["log", "-1", "--format=%B"])?
            .contains(&format!("{}:", trailer_name()))
    } else {
        true
    };
    Ok(json!({
        "id":id,"repo":f.repo,"literal":literal,"setupIdempotent":idempotent,"state":state,"local":local,"localMode":mode,
        "nativeHash":native_hash,"nativeAfter":capture::hash(fs::read(&transcript)?),"preRefs":pre_refs,"shadowRefs":shadows,"ignored":ignored,
        "unrelated":unrelated,"checkpoint":checkpoint,"tree":tree,"root":root,"metadata":metadata,"full":full,"compact":compact,
        "storedPrompt":stored_prompt,"contentHash":content_hash,"stagedHash":staged,"sourceTree":source_tree,
        "checkpointHash":checkpoint_hash,"sourceParents":source_parents,"checkpointParents":checkpoint_parents,
        "remoteHash":remote_hash,"fetchedHash":fetched_hash,"remainingLinked":remaining_linked
    }))
}
fn string<'a>(r: &'a Value, key: &str) -> Result<&'a str> {
    r[key]
        .as_str()
        .with_context(|| format!("Missing result {key}"))
}
fn check(index: usize, r: &Value, operation: &str, profile: &str) -> Result<()> {
    let s = |key| string(r, key);
    let literal = s("literal")?;
    let valid = match index {
        0 => r["setupIdempotent"] == true,
        1 => r["state"]["session_id"] == r["id"],
        2 => r["state"]["files_touched"]
            .as_array()
            .is_some_and(|v| !v.is_empty()),
        3 => s("local")?.contains(literal),
        4 => r["localMode"] == 0o600,
        5 => s("ignored")?.contains(&format!(".{}/metadata/", target())),
        6 => s("preRefs")?.is_empty(),
        7 => s("shadowRefs")?.is_empty(),
        8 => !s("unrelated")?.contains(&format!("{}:", trailer_name())),
        9 => capture::checkpoint_ref(s("checkpoint")?).is_ok(),
        10 => r["sourceTree"] == r["stagedHash"],
        11 => !s("sourceParents")?
            .split_whitespace()
            .any(|p| Some(p) == r["checkpointHash"].as_str()),
        12 => s("checkpointParents")?.split_whitespace().count() == 1,
        13 => [
            "metadata.json",
            "0/metadata.json",
            "0/full.jsonl",
            "0/transcript.jsonl",
            "0/prompt.txt",
            "0/content_hash.txt",
        ]
        .iter()
        .all(|p| s("tree").is_ok_and(|tree| tree.lines().any(|line| line == *p))),
        14 => r["root"]["checkpoint_id"] == r["checkpoint"],
        15 => r["metadata"]["session_id"] == r["id"],
        16 => redact::parse(s("full")?)?
            .iter()
            .any(|e| e["type"] == "user.message"),
        17 => !redact::parse(s("compact")?)?.is_empty(),
        18 => r["nativeAfter"] == r["nativeHash"],
        19 => {
            let retained = !["credential", "custom", "pii"].contains(&profile);
            ["full", "storedPrompt", "compact"]
                .iter()
                .all(|key| s(key).is_ok_and(|v| v.contains(literal) == retained))
        }
        20 => {
            redact::parse(s("full")?)?
                .iter()
                .find(|e| e["type"] == "session.start")
                .context("No session.start")?["data"]["context"]["cwd"]
                == r["repo"]
        }
        21 => regex::Regex::new("[a-f0-9]{64}")?.is_match(s("contentHash")?),
        22 => r["remoteHash"] == r["checkpointHash"],
        23 => r["fetchedHash"] == r["checkpointHash"],
        24 => {
            if operation == "partial" {
                r["remainingLinked"] == true
            } else {
                r["metadata"]["files_touched"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
            }
        }
        _ => false,
    };
    ensure!(valid, "Contract assertion failed: {}", CHECKS[index]);
    Ok(())
}
fn main() {
    let mut args = Arguments::from_args();
    args.test_threads = Some(1);
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut trials = vec![];
    for operation in [
        "add", "modify", "delete", "nested", "spaces", "unicode", "rename", "partial",
    ] {
        for profile in ["plain", "credential", "custom", "pii", "metadata"] {
            let fixture = Arc::new(OnceLock::<std::result::Result<Value, String>>::new());
            for (index, description) in CHECKS.iter().enumerate() {
                let fixture = fixture.clone();
                let results = results.clone();
                let name = format!("{operation}/{profile}: {description}");
                trials.push(Trial::test(name.clone(), move || {
                    let result = fixture
                        .get_or_init(|| scenario(operation, profile).map_err(|e| format!("{e:#}")));
                    let result = match result {
                        Ok(value) => {
                            check(index, value, operation, profile).map_err(|e| format!("{e:#}"))
                        }
                        Err(error) => Err(error.clone()),
                    };
                    results.lock().expect("results mutex").push(
                        json!({"name":name,"passed":result.is_ok(),"error":result.as_ref().err()}),
                    );
                    result.map_err(Into::into)
                }));
            }
        }
    }
    assert_eq!(trials.len(), 1000);
    let conclusion = libtest_mimic::run(&args, trials);
    let records = results.lock().expect("results mutex");
    report(&format!("{}-contract.json",target()), &json!({
        "target":target(),"implementation":"rust","timestamp":chrono::Utc::now().to_rfc3339(),"scenarios":40,
        "tests":records.len(),"passed":records.iter().filter(|r|r["passed"]==true).count(),"results":*records
    })).expect("Cannot persist contract evidence");
    conclusion.exit();
}
