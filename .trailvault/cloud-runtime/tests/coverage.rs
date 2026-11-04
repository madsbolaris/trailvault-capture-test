mod support;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use support::{Fixture, event, put};
use trailvault::{
    capture, cloud, codex_api,
    coverage::{self, Contract, FIELDS, Field, Source, Status},
    git::{self, Repo},
    native::Provider,
    redact,
};

const ID: &str = "00000000-0000-4000-8000-000000000001";

fn copilot() -> Result<String> {
    redact::jsonl(&[
        event("session.start", json!({"sessionId":ID})),
        event("user.message", json!({"content":"Run the tests"})),
        event(
            "assistant.message",
            json!({"content":"Done","model":"fixture","outputTokens":7}),
        ),
        event(
            "session.shutdown",
            json!({"usage":{"inputTokens":5},"cost":0.001}),
        ),
    ])
}

fn claude(cwd: &str) -> Result<String> {
    redact::jsonl(&[
        json!({"type":"user","uuid":"user-1","parentUuid":null,"timestamp":"2026-11-04T16:00:00Z","sessionId":ID,"cwd":cwd,"message":{"content":"Run the tests"}}),
        json!({"type":"assistant","uuid":"assistant-1","parentUuid":"user-1","sessionId":ID,"cwd":cwd,
            "message":{"model":"fixture","usage":{"input_tokens":5},"content":[{"type":"text","text":"Done"},{"type":"thinking","thinking":"Exposed synthetic summary"}]}}),
    ])
}

fn codex(cwd: &str) -> Result<String> {
    redact::jsonl(&[
        json!({"type":"session_meta","payload":{"id":ID,"cwd":cwd}}),
        json!({"type":"turn_context","payload":{"model":"fixture"}}),
        json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5}}}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Run the tests"}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}),
    ])
}

fn api() -> Value {
    let exchange = |id, method, result| {
        json!({"request":{"id":id,"method":method,"params":{"threadId":ID,"sortDirection":"asc"}},
        "response":{"id":id,"result":result}})
    };
    json!({"schema":"trailvault.codex-app-server-observation.v1","threadId":ID,"collectedAt":"2026-11-04T16:00:00Z",
        "collection":"authorized_app_server_rpc","responses":[
        exchange(1,"thread/read",json!({"thread":{"id":ID,"cwd":"/workspace","source":{"custom":"codex-cloud-agent"},
            "environments":[{"environmentId":"ccarenv_b64_U3ludGhldGljQ2xvdWRSdW50aW1lMTIzNDU"}]}})),
        exchange(2,"thread/turns/list",json!({"data":[{"id":"turn-1","status":"completed"}],"nextCursor":null})),
        exchange(3,"thread/items/list",json!({"data":[
            {"turnId":"turn-1","startedAtMs":123,"completedAtMs":456,"item":{"id":"user-1","type":"userMessage","content":[{"type":"text","text":"Run the tests"}]}},
            {"turnId":"turn-1","item":{"id":"assistant-1","type":"agentMessage","text":"Done"}},
            {"turnId":"turn-1","item":{"id":"reasoning-1","type":"reasoning","summary":[{"text":"Exposed synthetic summary"}]}},
            {"turnId":"turn-1","item":{"id":"future-1","type":"futureNativeItem","opaque":{"preserved":true}}}
        ],"nextCursor":null}))
    ]})
}

fn stored(repo: &Repo, reference: &str, name: &str) -> Result<String> {
    let output = git::run(
        &repo.root,
        &["show", &format!("{reference}:0/{name}")],
        &Default::default(),
        None,
    )?;
    ensure!(output.success, "Cannot read stored coverage evidence");
    Ok(output.stdout)
}

#[test]
fn all_six_checkpoint_paths_persist_hash_bound_coverage() -> Result<()> {
    for source in [
        Source::CopilotLocal,
        Source::CopilotCloud,
        Source::ClaudeLocal,
        Source::ClaudeCloud,
        Source::CodexLocal,
        Source::CodexCloud,
    ] {
        let mut f = Fixture::new()?;
        let repo = Repo::discover(&f.repo)?;
        f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":false,
            "publish_on_tool_completion":source==Source::CopilotCloud}}))?;
        let reference = match source {
            Source::CopilotLocal | Source::CopilotCloud => f.commit_capture()?.1,
            _ => {
                let hosted = matches!(source, Source::ClaudeCloud | Source::CodexCloud);
                let cwd = if hosted {
                    "/workspace"
                } else {
                    repo.root.to_str().context("Non-UTF8 fixture")?
                };
                let raw = match source {
                    Source::ClaudeLocal | Source::ClaudeCloud => claude(cwd)?,
                    Source::CodexLocal => codex(cwd)?,
                    Source::CodexCloud => serde_json::to_string(&api())?,
                    _ => unreachable!(),
                };
                let home = f.temp.path().join("native-provider");
                f.env
                    .insert("CLAUDE_CONFIG_DIR".into(), support::text(&home));
                f.env.insert("CODEX_HOME".into(), support::text(&home));
                let path = match source {
                    Source::ClaudeLocal => home.join("projects").join(format!("{ID}.jsonl")),
                    Source::CodexLocal => home.join("sessions").join(format!("rollout-{ID}.jsonl")),
                    _ => f.temp.path().join("selected-native.jsonl"),
                };
                put(&path, raw)?;
                let id = if hosted {
                    f.g(&[
                        "remote",
                        "add",
                        "origin",
                        "https://github.com/madsbolaris/trailvault-capture-test.git",
                    ])?;
                    capture::hosted_checkpoint(
                        &repo,
                        &json!({"native_session_id":ID,"transcript_path":path,"provenance":{
                        "remote_session_id":if source==Source::ClaudeCloud {"session_CoverageFixture"} else {ID},
                        "repository":"madsbolaris/trailvault-capture-test","source_commit":repo.g(&["rev-parse","HEAD"])?,
                        "source_branch":"test/coverage","transcript_cwd":cwd,"collected_at":"2026-11-04T16:00:00Z",
                        "collection":if source==Source::ClaudeCloud {"claude-teleport"} else {"codex-app-server"},
                        "completeness":if source==Source::ClaudeCloud {"handoff-may-be-compacted"} else {"available-persisted-history"},
                        "identity_basis":"collector_asserted"}}),
                    )?
                } else {
                    let provider = if source == Source::ClaudeLocal {
                        Provider::Claude
                    } else {
                        Provider::Codex
                    };
                    support::invoke(
                        &f.repo,
                        &["capture", provider.name(), "session-start", "-"],
                        &f.env,
                        Some(&json!({"sessionId":ID})),
                    )?;
                    support::invoke(
                        &f.repo,
                        &["capture", provider.name(), "agent-stop", "-"],
                        &f.env,
                        Some(&json!({"sessionId":ID,"transcriptPath":path})),
                    )?;
                    capture::native_checkpoint(&repo, ID)?
                };
                capture::checkpoint_ref(&id)?
            }
        };
        let metadata: Value = serde_json::from_str(&stored(&repo, &reference, "metadata.json")?)?;
        let contract: Contract = serde_json::from_value(metadata["capture_coverage"].clone())?;
        ensure!(
            contract.source == source
                && contract.version == 1
                && contract.fields.len() == FIELDS.len()
        );
        ensure!(contract.native_sha256 == capture::hash(stored(&repo, &reference, "full.jsonl")?));
        ensure!(
            contract.standardized_sha256
                == capture::hash(stored(&repo, &reference, "transcript.jsonl")?)
        );
        ensure!(contract.fields[&Field::UserMessages].standardized.status == Status::Captured);
        ensure!(contract.guarantees["live_presence"].status == Status::Unavailable);
        ensure!(contract.guarantees["original_history_complete"].status == Status::Unverified);
        if source == Source::CodexCloud {
            ensure!(contract.fields[&Field::Reasoning].native.status == Status::Captured);
            ensure!(contract.fields[&Field::Reasoning].standardized.status == Status::Unavailable);
        }
    }
    Ok(())
}

#[test]
fn retained_native_data_is_not_claimed_as_normalized_or_complete() -> Result<()> {
    let raw = copilot()?;
    let compact = redact::compact(&redact::parse(&raw)?)?;
    let report = coverage::build(Source::CopilotLocal, &raw, &raw, &compact)?;
    for field in [
        Field::EventIds,
        Field::Timestamps,
        Field::ParentLinks,
        Field::Model,
        Field::Usage,
        Field::Cost,
        Field::SessionIdentity,
    ] {
        ensure!(report.fields[&field].native.status == Status::Captured);
        ensure!(report.fields[&field].standardized.status == Status::Unavailable);
    }
    ensure!(report.fields[&Field::AttachmentPayloads].native.status == Status::Unverified);
    for claim in [
        "runtime_termination",
        "background_jobs_complete",
        "child_transcripts_complete",
        "attachments_complete",
    ] {
        ensure!(report.guarantees[claim].status == Status::Unverified);
    }
    Ok(())
}

#[test]
fn redaction_is_measured_without_publishing_source_values_or_guessing_from_markers() -> Result<()> {
    let raw = redact::jsonl(&[event(
        "user.message",
        json!({"content":"DB_PASSWORD=synthetic-coverage-secret"}),
    )])?;
    let mut redactor = redact::Redactor::new(&json!({}))?;
    let events: Vec<_> = redact::parse(&raw)?
        .iter()
        .map(|event| redactor.event(event))
        .collect();
    let report = coverage::build(
        Source::CopilotLocal,
        &raw,
        &redact::jsonl(&events)?,
        &redact::compact(&events)?,
    )?;
    ensure!(report.fields[&Field::UserMessages].native.status == Status::Redacted);
    ensure!(report.fields[&Field::UserMessages].standardized.status == Status::Redacted);
    ensure!(report.fields[&Field::UserMessages].native_values_changed_by_redaction == 1);
    ensure!(!serde_json::to_string(&report)?.contains("synthetic-coverage-secret"));
    let literal = redact::jsonl(&[event(
        "user.message",
        json!({"content":"REDACTED model cost usage compaction"}),
    )])?;
    let report = coverage::build(
        Source::CopilotLocal,
        &literal,
        &literal,
        &redact::compact(&redact::parse(&literal)?)?,
    )?;
    ensure!(report.fields[&Field::UserMessages].native.status == Status::Captured);
    for field in [
        Field::Model,
        Field::Usage,
        Field::Cost,
        Field::CompactionEvents,
    ] {
        ensure!(report.fields[&field].native.status == Status::Unverified);
    }
    Ok(())
}

#[test]
fn references_are_not_attachment_payloads_or_child_transcripts() -> Result<()> {
    let raw = redact::jsonl(&[
        event(
            "user.message",
            json!({"content":"Read the file","attachments":[{"path":"/fixture/image.png"}]}),
        ),
        event(
            "subagent.started",
            json!({"toolCallId":"child-1","agentId":"agent-1"}),
        ),
        event(
            "session.compaction_complete",
            json!({"summary":"Synthetic compaction"}),
        ),
        event("future.event", json!({"opaque":{"preserved":true}})),
    ])?;
    let report = coverage::build(
        Source::CopilotLocal,
        &raw,
        &raw,
        &redact::compact(&redact::parse(&raw)?)?,
    )?;
    ensure!(report.fields[&Field::AttachmentReferences].native.status == Status::Captured);
    ensure!(report.fields[&Field::AttachmentPayloads].native.status == Status::Unverified);
    ensure!(report.fields[&Field::AgentCoordination].native.status == Status::Captured);
    ensure!(report.guarantees["child_transcripts_complete"].status == Status::Unverified);
    ensure!(report.fields[&Field::CompactionEvents].native.status == Status::Captured);
    ensure!(report.guarantees["original_history_complete"].status == Status::Unverified);
    ensure!(report.fields[&Field::NativeRecords].native.observed_values == 4);
    Ok(())
}

#[test]
fn copilot_cloud_validation_rejects_forged_contracts_but_accepts_legacy_absence() -> Result<()> {
    let f = Fixture::new()?;
    f.remote()?;
    f.configure(&json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":true}}))?;
    f.commit_capture()?;
    let path = f.temp.path().join("export.json");
    cloud::export(&Repo::discover(&f.repo)?, "owner/repo", 123, 1, &path)?;
    let mut artifact: cloud::Artifact = git::read(&path)?;
    let checkpoint = &mut artifact.checkpoints[0];
    cloud::validate(checkpoint)?;
    let original: Value = serde_json::from_str(&checkpoint.files["0/metadata.json"])?;
    type Mutation = fn(&mut Value);
    let mutations: &[Mutation] = &[
        |v| v["capture_coverage"]["version"] = json!(2),
        |v| v["capture_coverage"]["native_sha256"] = json!("wrong"),
        |v| v["capture_coverage"]["source"] = json!("codex_cloud"),
        |v| {
            v["capture_coverage"]["fields"]["user_messages"]["native"]["observed_values"] =
                json!(999)
        },
        |v| {
            v["capture_coverage"]["fields"]["user_messages"]["native_values_changed_by_redaction"] =
                json!(999)
        },
        |v| v["capture_coverage"]["guarantees"]["live_presence"]["status"] = json!("captured"),
        |v| {
            v["capture_coverage"]["fields"]
                .as_object_mut()
                .unwrap()
                .remove("usage");
        },
        |v| v["capture_coverage"] = Value::Null,
    ];
    for mutation in mutations {
        let mut metadata = original.clone();
        mutation(&mut metadata);
        checkpoint
            .files
            .insert("0/metadata.json".into(), serde_json::to_string(&metadata)?);
        ensure!(
            cloud::validate(checkpoint).is_err(),
            "Forged coverage accepted"
        );
    }
    let mut legacy = original;
    legacy.as_object_mut().unwrap().remove("capture_coverage");
    checkpoint
        .files
        .insert("0/metadata.json".into(), serde_json::to_string(&legacy)?);
    cloud::validate(checkpoint)?;
    Ok(())
}

#[test]
fn replayed_api_pages_are_counted_as_observations_not_unique_events() -> Result<()> {
    let source = api();
    let mut replay = source.clone();
    replay["responses"]
        .as_array_mut()
        .unwrap()
        .push(source["responses"][2].clone());
    let raw = codex_api::normalize(&serde_json::to_string(&replay)?, ID, "2026-11-04T16:00:00Z")?;
    let parsed = codex_api::parse(ID, std::path::Path::new("/workspace"), &raw)?.0;
    let compact = redact::compact(&parsed.canonical)?;
    let report = coverage::build(Source::CodexCloud, &raw, &raw, &compact)?;
    ensure!(report.fields[&Field::UserMessages].native.observed_values == 2);
    ensure!(
        report.fields[&Field::UserMessages]
            .standardized
            .observed_values
            == 1
    );
    ensure!(report.fields[&Field::Reasoning].native.observed_values == 2);
    ensure!(report.fields[&Field::Reasoning].standardized.status == Status::Unavailable);
    Ok(())
}

#[test]
fn empty_reasoning_and_usage_containers_do_not_claim_exposed_content() -> Result<()> {
    let mut source = api();
    source["responses"][2]["response"]["result"]["data"][2]["item"]["summary"] = json!([]);
    source["responses"][2]["response"]["result"]["data"][2]["item"]["content"] = json!([]);
    source["responses"][1]["response"]["result"]["data"][0]["usage"] = json!({});
    let raw = codex_api::normalize(&serde_json::to_string(&source)?, ID, "2026-11-04T16:00:00Z")?;
    let parsed = codex_api::parse(ID, std::path::Path::new("/workspace"), &raw)?.0;
    let report = coverage::build(
        Source::CodexCloud,
        &raw,
        &raw,
        &redact::compact(&parsed.canonical)?,
    )?;
    ensure!(report.fields[&Field::Reasoning].native.status == Status::Unverified);
    ensure!(report.fields[&Field::Usage].native.status == Status::Unverified);
    ensure!(report.fields[&Field::NativeRecords].native.status == Status::Captured);
    Ok(())
}
