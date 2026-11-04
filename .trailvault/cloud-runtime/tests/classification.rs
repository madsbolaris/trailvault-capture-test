use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use trailvault::{native, redact};

#[derive(Clone)]
struct Scenario {
    name: &'static str,
    kind: &'static str,
    text: &'static str,
    arguments: Value,
    provenance: &'static str,
    activity: &'static str,
}

fn scenarios() -> Vec<Scenario> {
    let mut cases = Vec::new();
    for (name, kind, text, provenance, activity) in [
        (
            "research-prompt",
            "user",
            "Research the API",
            "user_prompts",
            "research",
        ),
        (
            "planning-prompt",
            "user",
            "Plan the migration",
            "user_prompts",
            "planning",
        ),
        (
            "implementation-prompt",
            "user",
            "Implement the parser",
            "user_prompts",
            "implementation",
        ),
        (
            "validation-prompt",
            "user",
            "Run the tests",
            "user_prompts",
            "validation",
        ),
        (
            "debugging-prompt",
            "user",
            "Debug the crash",
            "user_prompts",
            "debugging",
        ),
        (
            "delivery-prompt",
            "user",
            "Publish the release",
            "user_prompts",
            "delivery",
        ),
        (
            "ambiguous-prompt",
            "user",
            "Hello",
            "user_prompts",
            "unknown",
        ),
        ("empty-prompt", "user", "", "user_prompts", "unknown"),
        (
            "mixed-prompt",
            "user",
            "Research the API and implement the parser",
            "user_prompts",
            "unknown",
        ),
        (
            "assistant-prose",
            "assistant",
            "Done. Research test publish are quoted words.",
            "assistant_responses",
            "unknown",
        ),
        (
            "empty-assistant",
            "assistant",
            "",
            "assistant_responses",
            "unknown",
        ),
        (
            "system-instructions",
            "system",
            "Follow repository rules",
            "system_instructions",
            "unknown",
        ),
        (
            "exposed-reasoning",
            "reasoning",
            "Synthetic reasoning fixture",
            "reasoning",
            "unknown",
        ),
    ] {
        cases.push(Scenario {
            name,
            kind,
            text,
            arguments: Value::Null,
            provenance,
            activity,
        });
    }
    for (name, tool, arguments, provenance, activity) in [
        (
            "read-file",
            "Read",
            json!({"file_path":"src/lib.rs"}),
            "files_retrieved",
            "research",
        ),
        (
            "copilot-view",
            "view",
            json!({"path":"src/main.rs"}),
            "files_retrieved",
            "research",
        ),
        (
            "codex-read",
            "read_file",
            json!({"path":"README.md"}),
            "files_retrieved",
            "research",
        ),
        (
            "instructions-agents",
            "Read",
            json!({"file_path":"AGENTS.md"}),
            "system_instructions",
            "research",
        ),
        (
            "instructions-claude",
            "view",
            json!({"path":"/repo/CLAUDE.md"}),
            "system_instructions",
            "research",
        ),
        (
            "instructions-copilot",
            "read_file",
            json!({"path":".github/copilot-instructions.md"}),
            "system_instructions",
            "research",
        ),
        (
            "skill-file",
            "Read",
            json!({"file_path":".github/skills/review/SKILL.md"}),
            "skills_tools",
            "research",
        ),
        (
            "grep-search",
            "grep",
            json!({"pattern":"parse"}),
            "files_retrieved",
            "research",
        ),
        (
            "glob-search",
            "glob",
            json!({"pattern":"*.rs"}),
            "files_retrieved",
            "research",
        ),
        (
            "web-search",
            "web_search",
            json!({"query":"Rust serde"}),
            "files_retrieved",
            "research",
        ),
        (
            "web-fetch",
            "web_fetch",
            json!({"url":"https://example.invalid"}),
            "files_retrieved",
            "research",
        ),
        (
            "write-file",
            "Write",
            json!({"file_path":"src/lib.rs","content":"new"}),
            "tool_interactions",
            "implementation",
        ),
        (
            "edit-file",
            "edit",
            json!({"path":"src/lib.rs"}),
            "tool_interactions",
            "implementation",
        ),
        (
            "apply-patch",
            "apply_patch",
            json!("*** Begin Patch\n*** End Patch"),
            "tool_interactions",
            "implementation",
        ),
        (
            "cargo-test",
            "Bash",
            json!({"command":"cargo test --locked"}),
            "tool_interactions",
            "validation",
        ),
        (
            "cargo-check",
            "bash",
            json!({"command":"cargo check"}),
            "tool_interactions",
            "validation",
        ),
        (
            "npm-test",
            "exec_command",
            json!({"cmd":"npm test"}),
            "tool_interactions",
            "validation",
        ),
        (
            "npm-lint",
            "bash",
            json!({"command":"npm run lint"}),
            "tool_interactions",
            "validation",
        ),
        (
            "pytest",
            "Bash",
            json!({"command":"pytest tests/"}),
            "tool_interactions",
            "validation",
        ),
        (
            "git-status",
            "Bash",
            json!({"command":"git status --short"}),
            "tool_interactions",
            "research",
        ),
        (
            "git-commit",
            "Bash",
            json!({"command":"git commit -m release"}),
            "tool_interactions",
            "delivery",
        ),
        (
            "debugger",
            "bash",
            json!({"command":"lldb ./target/debug/app"}),
            "tool_interactions",
            "debugging",
        ),
        (
            "opaque-wrapper",
            "exec",
            json!("text(await tools.exec_command({cmd:'cargo test'}));"),
            "tool_interactions",
            "unknown",
        ),
        (
            "echo-not-a-test",
            "bash",
            json!({"command":"echo 'cargo test'"}),
            "tool_interactions",
            "unknown",
        ),
        (
            "compound-command",
            "bash",
            json!({"command":"cargo test && git push"}),
            "tool_interactions",
            "unknown",
        ),
        (
            "future-tool",
            "future.tool",
            json!({"content":"Run the tests"}),
            "tool_interactions",
            "unknown",
        ),
        (
            "delegate-task",
            "task",
            json!({"prompt":"Inspect code","agent_type":"explore"}),
            "agent_coordination",
            "unknown",
        ),
    ] {
        cases.push(Scenario {
            name,
            kind: tool,
            text: "Captured result \u{65e5}\u{672c}\nsecond line",
            arguments,
            provenance,
            activity,
        });
    }
    assert_eq!(cases.len(), 40);
    cases
}

fn convert(case: &Scenario, path: usize) -> Result<(Vec<Value>, Vec<Value>)> {
    let message = ["user", "assistant", "system", "reasoning"].contains(&case.kind);
    let mut events = if message {
        vec![json!({"type":match case.kind {
            "user"=>"user.message", "assistant"=>"assistant.message",
            "system"=>"system.message", _=>"assistant.reasoning"},
            "data":{"content":case.text}})]
    } else {
        let start = if path == 1 {
            json!({"type":"assistant.message","data":{"content":"","toolRequests":[{
                "toolCallId":"call-1","name":case.kind,"arguments":case.arguments}]}})
        } else {
            json!({"type":"tool.execution_start","data":{"toolCallId":"call-1","toolName":case.kind,"arguments":case.arguments}})
        };
        vec![
            start,
            json!({"type":"tool.execution_complete","data":{"toolCallId":"call-1",
            "success":true,"result":{"content":case.text}}}),
        ]
    };
    if path >= 2 {
        let cwd = std::env::current_dir()?.canonicalize()?;
        let id = "10000000-0000-4000-8000-000000000001";
        let provider = if path == 2 {
            native::Provider::Claude
        } else {
            native::Provider::Codex
        };
        let mut raw = if provider == native::Provider::Claude {
            vec![json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":[]}})]
        } else {
            vec![json!({"type":"session_meta","payload":{"id":id,"cwd":cwd}})]
        };
        if provider == native::Provider::Claude {
            if message {
                raw.push(if case.kind == "system" {
                    json!({"type":"system","sessionId":id,"cwd":cwd,"content":case.text})
                } else {
                    let block = if case.kind == "reasoning" { json!({"type":"thinking","thinking":case.text}) }
                        else { json!({"type":"text","text":case.text}) };
                    json!({"type":if case.kind=="user" {"user"} else {"assistant"},"sessionId":id,"cwd":cwd,"message":{"content":[block]}})
                });
            } else {
                raw.push(
                    json!({"type":"assistant","sessionId":id,"cwd":cwd,"message":{"content":[{
                    "type":"tool_use","id":"call-1","name":case.kind,"input":case.arguments}]}}),
                );
                raw.push(
                    json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":[{
                    "type":"tool_result","tool_use_id":"call-1","content":case.text}]}}),
                );
            }
        } else if message {
            let payload = if case.kind == "reasoning" {
                json!({"type":"reasoning","summary":[{"type":"summary_text","text":case.text}]})
            } else {
                json!({"type":"message","role":if case.kind=="system" {"developer"} else {case.kind},
                    "content":[{"type":"input_text","text":case.text}]})
            };
            raw.push(json!({"type":"response_item","payload":payload}));
        } else {
            let custom = path == 4;
            raw.push(json!({"type":"response_item","payload":{
                "type":if custom {"custom_tool_call"} else {"function_call"},
                "call_id":"call-1","name":case.kind,"arguments":case.arguments,"input":case.arguments}}));
            raw.push(json!({"type":"response_item","payload":{
                "type":if custom {"custom_tool_call_output"} else {"function_call_output"},
                "call_id":"call-1","output":case.text}}));
        }
        events = native::parse(provider, id, &cwd, &redact::jsonl(&raw)?)?.canonical;
    }
    let before = events.clone();
    let rows = redact::parse(&redact::compact(&events)?)?;
    ensure!(events == before, "Conversion mutated source events");
    Ok((events, rows))
}

fn check(case: &Scenario, path: usize, assertion: usize) -> Result<()> {
    let (events, rows) = convert(case, path)?;
    let row = rows
        .iter()
        .rev()
        .find(|row| row["type"] != "header")
        .context("No classified record emitted")?;
    match assertion {
        0 => ensure!(
            row["classification"]["provenance"] == case.provenance,
            "Wrong provenance: {row}"
        ),
        1 => ensure!(
            row["classification"]["activity"] == case.activity,
            "Wrong activity: {row}"
        ),
        2 => {
            ensure!(
                rows[0]["version"] == 2 && rows[0]["classification_version"] == 2,
                "Missing versioned contract"
            );
            for record in rows.iter().skip(1) {
                let class = &record["classification"];
                ensure!(
                    class["version"] == 2 && class["method"] == "deterministic",
                    "Missing classification method/version"
                );
                ensure!(
                    class["basis"].is_string() && class["activity_basis"].is_string(),
                    "Missing evidence basis"
                );
                ensure!(
                    class
                        .as_object()
                        .context("Classification object required")?
                        .len()
                        == 6,
                    "Unexpected classification payload"
                );
            }
        }
        3 => {
            ensure!(
                serde_json::to_string(row)?.contains(&serde_json::to_string(case.text)?),
                "Captured payload changed"
            );
            ensure!(
                redact::compact(&events)? == redact::compact(&events)?,
                "Classification is nondeterministic"
            );
            ensure!(
                row["classification"].to_string().len() < 300,
                "Classification copied transcript content"
            );
        }
        4 => {
            if !["user", "assistant", "system", "reasoning"].contains(&case.kind) {
                let starts: Vec<_> = rows
                    .iter()
                    .filter(|r| r["type"] == "tool.execution_start")
                    .collect();
                ensure!(starts.len() == 1, "Tool request missing or duplicated");
                ensure!(
                    starts[0]["data"]["toolName"] == case.kind
                        && starts[0]["data"]["arguments"] == case.arguments,
                    "Tool input changed"
                );
                ensure!(
                    row["data"]["toolCallId"] == starts[0]["data"]["toolCallId"],
                    "Tool correlation lost"
                );
                ensure!(
                    starts[0]["classification"]["activity"] == case.activity,
                    "Call and result activity disagree"
                );
            } else {
                ensure!(rows.len() == 2, "Message/context duplicated or omitted");
                ensure!(row["content"] == case.text, "Content not retained");
            }
        }
        _ => anyhow::bail!("Unknown assertion"),
    }
    Ok(())
}

fn main() {
    let paths = [
        "copilot-lifecycle",
        "copilot-request-only",
        "claude",
        "codex-function",
        "codex-custom",
    ];
    let checks = [
        "provenance",
        "activity",
        "versioned-shape",
        "payload-and-repeatability",
        "correlation",
    ];
    let records = Arc::new(Mutex::new(Vec::new()));
    let mut trials = Vec::new();
    for case in scenarios() {
        for (path, label) in paths.iter().enumerate() {
            for (assertion, description) in checks.iter().enumerate() {
                let case = case.clone();
                let records = records.clone();
                let name = format!("{label}/{}: {description}", case.name);
                trials.push(Trial::test(name.clone(), move || {
                    let result = check(&case, path, assertion).map_err(|e| format!("{e:#}"));
                    records
                        .lock()
                        .expect("classification results lock")
                        .push(json!({
                        "name":name,"passed":result.is_ok(),"error":result.as_ref().err()}));
                    result.map_err(Into::into)
                }));
            }
        }
    }
    assert_eq!(trials.len(), 1000);
    let result = libtest_mimic::run(&Arguments::from_args(), trials);
    let records = records.lock().expect("classification results lock");
    let report = json!({"planned_tests":1000,"executed_tests":records.len(),
        "passed":records.iter().filter(|r|r["passed"]==true).count(),
        "qualification":"40 behavioral scenarios x 5 ingestion paths x 5 contract checks; synthetic, no model inference",
        "results":*records});
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".test-results");
    std::fs::create_dir_all(&root).expect("Create report directory");
    std::fs::write(
        root.join("classification.json"),
        serde_json::to_vec_pretty(&report).expect("Encode report"),
    )
    .expect("Persist report");
    result.exit();
}
