use anyhow::{Result, ensure};
use serde_json::{Value, json};
use trailvault::{capture, cloud, native, redact};

fn rows(events: &[Value]) -> Result<Vec<Value>> {
    redact::parse(&redact::compact(events)?)
}

fn start(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type":"tool.execution_start","data":{"toolCallId":id,"toolName":name,"arguments":arguments}})
}

fn finish(id: &str, success: bool) -> Value {
    json!({"type":"tool.execution_complete","data":{"toolCallId":id,"success":success,"result":{"content":"result"}}})
}

#[test]
fn parallel_and_reversed_results_use_call_identity_not_adjacency() -> Result<()> {
    let output = rows(&[
        finish("read", true),
        start("test", "bash", json!({"command":"cargo test"})),
        start("read", "Read", json!({"file_path":"AGENTS.md"})),
        finish("test", false),
    ])?;
    ensure!(output[1]["classification"]["provenance"] == "system_instructions");
    ensure!(output[4]["classification"]["activity"] == "validation");
    Ok(())
}

#[test]
fn failed_read_is_tool_error_not_retrieved_file_content() -> Result<()> {
    let output = rows(&[
        start("read", "Read", json!({"file_path":"AGENTS.md"})),
        finish("read", false),
    ])?;
    ensure!(output[2]["classification"]["provenance"] == "tool_interactions");
    ensure!(output[2]["classification"]["activity"] == "research");
    Ok(())
}

#[test]
fn instructions_and_retained_memory_have_distinct_provenance() -> Result<()> {
    let output = rows(&[
        json!({"type":"system.message","data":{"content":"Follow these rules"}}),
        start("instructions", "Read", json!({"path":"AGENTS.md"})),
        finish("instructions", true),
        start("memory", "Read", json!({"path":"/repo/memory/MEMORY.md"})),
        finish("memory", true),
        start(
            "store",
            "store_memory",
            json!({"fact":"Use the shared formatter"}),
        ),
        finish("store", true),
        start("failed", "read_memory", json!({"id":"missing"})),
        finish("failed", false),
    ])?;
    ensure!(output[0]["classification_version"] == 2);
    ensure!(output[1]["classification"]["provenance"] == "system_instructions");
    ensure!(output[3]["classification"]["provenance"] == "system_instructions");
    ensure!(output[5]["classification"]["provenance"] == "memory");
    ensure!(output[5]["classification"]["basis"] == "tool_path");
    ensure!(output[6]["classification"]["provenance"] == "memory");
    ensure!(output[6]["classification"]["basis"] == "tool_name");
    ensure!(output[9]["classification"]["provenance"] == "tool_interactions");
    Ok(())
}

#[test]
fn orphan_and_empty_tools_remain_explicitly_unknown() -> Result<()> {
    for data in [
        json!({"toolCallId":"orphan"}),
        json!({}),
        json!({"toolCallId":""}),
    ] {
        let output = rows(&[json!({"type":"tool.execution_complete","data":data})])?;
        ensure!(output[1]["classification"]["activity"] == "unknown");
        ensure!(output[1]["data"] == data);
    }
    Ok(())
}

#[test]
fn native_and_embedded_tool_descriptions_emit_only_one_start() -> Result<()> {
    let request = json!({"type":"assistant.message","data":{"toolRequests":[{
        "toolCallId":"one","name":"Read","arguments":{"path":"src/lib.rs"}}]}});
    let output = rows(&[
        request.clone(),
        request,
        start("one", "Read", json!({"path":"src/lib.rs"})),
        finish("one", true),
    ])?;
    ensure!(
        output
            .iter()
            .filter(|row| row["type"] == "tool.execution_start")
            .count()
            == 1
    );
    Ok(())
}

#[test]
fn conflicting_call_evidence_is_an_error_not_an_arbitrary_label() {
    assert!(
        rows(&[
            start("one", "Read", json!({"path":"a"})),
            start("one", "Write", json!({"path":"b"})),
        ])
        .is_err()
    );
}

#[test]
fn repeated_embedded_requests_with_conflicting_arguments_are_rejected() {
    let request = |path| {
        json!({"type":"assistant.message","data":{"toolRequests":[{
        "toolCallId":"one","name":"Read","arguments":{"path":path}}]}})
    };
    assert!(rows(&[request("a"), request("b")]).is_err());
}

#[test]
fn embedded_request_and_lifecycle_conflicts_are_rejected() {
    let request = json!({"type":"assistant.message","data":{"toolRequests":[{
        "toolCallId":"one","name":"Read","arguments":{"path":"a"}}]}});
    assert!(rows(&[request, start("one", "Write", json!({"path":"b"}))]).is_err());
}

#[test]
fn json_encoded_arguments_and_structured_arguments_classify_equally() -> Result<()> {
    let input = json!({"command":"cargo test"});
    let direct = rows(&[start("one", "Bash", input.clone()), finish("one", true)])?;
    let encoded = rows(&[
        start("one", "Bash", json!(serde_json::to_string(&input)?)),
        finish("one", true),
    ])?;
    ensure!(direct[2]["classification"] == encoded[2]["classification"]);
    Ok(())
}

#[test]
fn shell_literals_substitutions_and_wrappers_are_not_executed_or_guessed() -> Result<()> {
    for command in [
        "echo 'cargo test'",
        "sh -c 'cargo test'",
        "eval cargo test",
        "cargo test && git push",
        "cargo test\n git push",
        "$(cargo test)",
        "`cargo test`",
        "cargo test > output",
        "cargo test | cat",
    ] {
        let output = rows(&[start("one", "Bash", json!({"command":command}))])?;
        ensure!(
            output[1]["classification"]["activity"] == "unknown",
            "{command}"
        );
    }
    Ok(())
}

#[test]
fn ambiguous_negated_and_quoted_intent_abstains() -> Result<()> {
    for text in [
        "Do not publish the release",
        "Don't implement this",
        "Never push",
        "Research and implement it",
        "The log says \"publish\"",
        "```\ngit push\n```",
        "Do we need to deploy?",
    ] {
        let output = rows(&[json!({"type":"user.message","data":{"content":text}})])?;
        ensure!(
            output[1]["classification"]["activity"] == "unknown",
            "{text}"
        );
    }
    Ok(())
}

#[test]
fn user_intent_resets_and_tool_output_does_not_inject_activity() -> Result<()> {
    let output = rows(&[
        json!({"type":"user.message","data":{"content":"Implement the parser"}}),
        json!({"type":"assistant.message","data":{"content":"Working"}}),
        json!({"type":"user.message","data":{"content":"Thanks"}}),
        json!({"type":"assistant.message","data":{"content":"Publish it now"}}),
    ])?;
    ensure!(output[2]["classification"]["activity"] == "implementation");
    ensure!(output[2]["classification"]["activity_basis"] == "user_intent");
    ensure!(output[4]["classification"]["activity"] == "unknown");
    Ok(())
}

#[test]
fn redaction_precedes_classification_and_does_not_reintroduce_content() -> Result<()> {
    let raw = json!({"type":"user.message","data":{"content":"Implement PRIVATE-PROJECT"}});
    let mut redactor = redact::Redactor::new(
        &json!({"redaction":{"custom_redactions":{"project":"PRIVATE-PROJECT"}}}),
    )?;
    let redacted = redactor.event(&raw);
    let compact = redact::compact(&[redacted])?;
    ensure!(!compact.contains("PRIVATE-PROJECT"));
    ensure!(compact.contains("REDACTED"));
    ensure!(raw["data"]["content"] == "Implement PRIVATE-PROJECT");
    Ok(())
}

#[test]
fn context_kinds_and_unknown_structured_content_are_not_prompts() -> Result<()> {
    let output = rows(&[
        json!({"type":"skill.activated","data":{"content":"review"}}),
        json!({"type":"tools.catalog","data":{"content":[{"name":"Read"}]}}),
        json!({"type":"system.message","data":{"content":"Publish everything"}}),
        json!({"type":"user.message","data":{"content":{"image":"reference"}}}),
    ])?;
    ensure!(output[1]["classification"]["provenance"] == "skills_tools");
    ensure!(output[2]["content"].is_array());
    ensure!(output[3]["classification"]["activity"] == "unknown");
    ensure!(output[4]["classification"]["activity"] == "unknown");
    Ok(())
}

#[test]
fn child_links_survive_and_do_not_create_child_transcripts_or_usage() -> Result<()> {
    let output = rows(&[
        json!({"type":"subagent.started","agentId":"child","data":{"toolCallId":"task"}}),
        start("task", "task", json!({"agent_type":"explore"})),
        finish("task", true),
    ])?;
    ensure!(output[2]["data"]["result"]["agent"]["id"] == "child");
    ensure!(output[2]["classification"]["provenance"] == "agent_coordination");
    ensure!(!serde_json::to_string(&output)?.contains("token_usage"));
    Ok(())
}

#[test]
fn independent_snapshots_do_not_accumulate_previous_classifications() -> Result<()> {
    let short = vec![json!({"type":"user.message","data":{"content":"Run the tests"}})];
    let first = rows(&short)?;
    let mut long = short.clone();
    long.push(json!({"type":"user.message","data":{"content":"Hello"}}));
    rows(&long)?;
    ensure!(rows(&short)? == first);
    Ok(())
}

#[test]
fn cloud_accepts_historical_v1_and_recomputes_v2_classifications() -> Result<()> {
    let id = "10000000-0000-4000-8000-000000000001";
    let events = vec![
        json!({"type":"session.start","data":{"sessionId":id}}),
        json!({"type":"system.message","data":{"content":"Follow repository instructions"}}),
        json!({"type":"user.message","data":{"content":"Run the tests"}}),
        json!({"type":"assistant.message","data":{"content":"Done"}}),
    ];
    let full = redact::jsonl(&events)?;
    let mut checkpoint = cloud::Checkpoint {
        id: capture::checkpoint_id(),
        source_commit: "a".repeat(40),
        files: Default::default(),
    };
    checkpoint.files.insert("metadata.json".into(), json!({"checkpoint_id":checkpoint.id,"sessions":[{
        "metadata":"/0/metadata.json","transcript":"/0/full.jsonl",
        "compact_transcript":"/0/transcript.jsonl","prompt":"/0/prompt.txt","content_hash":"/0/content_hash.txt"}]}).to_string());
    checkpoint.files.insert(
        "0/metadata.json".into(),
        json!({"checkpoint_id":checkpoint.id,"session_id":id}).to_string(),
    );
    checkpoint
        .files
        .insert("0/prompt.txt".into(), "Run the tests\n".into());
    checkpoint.files.insert(
        "0/content_hash.txt".into(),
        format!("sha256:{}\n", capture::hash(&full)),
    );
    checkpoint.files.insert("0/full.jsonl".into(), full);
    let mut legacy = rows(&events)?;
    legacy[0]["classification_version"] = json!(1);
    for row in legacy.iter_mut().skip(1) {
        row["classification"]["version"] = json!(1);
        if row["classification"]["provenance"] == "system_instructions" {
            row["classification"]["provenance"] = json!("instructions_memory");
        }
    }
    for compact in [
        redact::compact_v1(&events)?,
        redact::jsonl(&legacy)?,
        redact::compact(&events)?,
    ] {
        checkpoint
            .files
            .insert("0/transcript.jsonl".into(), compact);
        cloud::validate(&checkpoint)?;
    }
    let mut forged = rows(&events)?;
    forged[1]["classification"]["activity"] = json!("delivery");
    checkpoint
        .files
        .insert("0/transcript.jsonl".into(), redact::jsonl(&forged)?);
    ensure!(cloud::validate(&checkpoint).is_err());
    forged[0]["version"] = json!(999);
    checkpoint
        .files
        .insert("0/transcript.jsonl".into(), redact::jsonl(&forged)?);
    ensure!(cloud::validate(&checkpoint).is_err());
    Ok(())
}

#[test]
fn codex_reasoning_summary_remains_labeled_as_a_summary() -> Result<()> {
    let cwd = std::env::current_dir()?.canonicalize()?;
    let id = "10000000-0000-4000-8000-000000000001";
    let raw = redact::jsonl(&[
        json!({"type":"session_meta","payload":{"id":id,"cwd":cwd}}),
        json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"text":"Synthetic summary"}]}}),
    ])?;
    let canonical = native::parse(native::Provider::Codex, id, &cwd, &raw)?.canonical;
    let output = rows(&canonical)?;
    ensure!(output[1]["representation"] == "summary");
    ensure!(output[1]["classification"]["provenance"] == "reasoning");
    Ok(())
}
