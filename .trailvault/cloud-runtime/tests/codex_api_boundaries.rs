mod support;

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
use trailvault::{capture, codex_api, git, redact};

const ID: &str = "00000000-0000-4000-8000-000000000001";

fn trace() -> Value {
    let exchange = |id, method, params, result| json!({"request":{"id":id,"method":method,"params":params},"response":{"id":id,"result":result}});
    json!({"schema":"trailvault.codex-app-server-observation.v1","threadId":ID,
        "collectedAt":chrono::Utc::now().to_rfc3339(),"collection":"authorized_app_server_rpc","responses":[
        exchange(0,"thread/read",json!({"threadId":ID}),json!({"thread":{"id":ID,"cwd":"/workspace",
            "source":{"custom":"codex-cloud-agent"},"status":{"type":"idle"},"environments":[{"environmentId":"ccarenv_b64_U3ludGhldGljQ2xvdWRSdW50aW1lMTIzNDU"}]}})),
        exchange(1,"thread/turns/list",json!({"threadId":ID,"sortDirection":"asc"}),json!({"data":[{"id":"turn-1","status":"completed"}],"nextCursor":null})),
        exchange(2,"thread/items/list",json!({"threadId":ID,"sortDirection":"asc"}),json!({"data":[
            {"turnId":"turn-1","item":{"id":"user-1","type":"userMessage","content":[{"type":"text","text":"Synthetic API boundary"}]}},
            {"turnId":"turn-1","item":{"id":"call-1","type":"commandExecution","command":"git diff --check","cwd":"/workspace","status":"completed","exitCode":0,"aggregatedOutput":""}}
        ],"nextCursor":null}))
    ]})
}
fn normalized(source: &Value) -> Result<String> {
    codex_api::normalize(
        &serde_json::to_string(source)?,
        ID,
        source["collectedAt"].as_str().unwrap(),
    )
}
fn check(source: &Value) -> Result<()> {
    codex_api::parse(ID, Path::new("/workspace"), &normalized(source)?)?;
    Ok(())
}

#[test]
fn complete_per_turn_history_accepts_bounded_previews_but_not_missing_turns() -> Result<()> {
    let mut source = trace();
    source["responses"][2]["request"]["params"]["turnId"] = json!("turn-1");
    let mut preview = source["responses"][2].clone();
    preview["request"]["params"]["sortDirection"] = json!("desc");
    preview["response"]["result"]["data"]
        .as_array_mut()
        .unwrap()
        .reverse();
    preview["response"]["result"]["data"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    preview["response"]["result"]["nextCursor"] = json!("uncollected-preview");
    source["responses"].as_array_mut().unwrap().push(preview);
    let (_, summary) = codex_api::parse(ID, Path::new("/workspace"), &normalized(&source)?)?;
    ensure!(
        summary["item_count"] == 2
            && summary["pagination_scope"] == "per_turn"
            && summary["incomplete_preview_chains"] == 1
    );
    source["responses"][1]["response"]["result"]["data"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"turn-2","status":"completed"}));
    ensure!(
        check(&source).is_err(),
        "Missing second-turn history was accepted"
    );
    let mut same_direction = trace();
    same_direction["responses"][2]["request"]["params"]["turnId"] = json!("turn-1");
    same_direction["responses"][2]["request"]["params"]["limit"] = json!(500);
    let mut preview = same_direction["responses"][2].clone();
    preview["request"]["params"]["limit"] = json!(2);
    preview["response"]["result"]["data"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    preview["response"]["result"]["nextCursor"] = json!("uncollected-preview");
    same_direction["responses"]
        .as_array_mut()
        .unwrap()
        .push(preview);
    check(&same_direction)?;
    Ok(())
}

#[test]
fn malformed_or_incomplete_protocol_never_becomes_a_successful_archive() -> Result<()> {
    type Mutation = fn(&mut Value);
    let cases: &[(&str, Mutation, &str)] = &[
        (
            "foreign request",
            |t| t["responses"][2]["request"]["params"]["threadId"] = json!("foreign"),
            "different session",
        ),
        (
            "foreign response",
            |t| t["responses"][0]["response"]["result"]["thread"]["id"] = json!("foreign"),
            "different session",
        ),
        (
            "mismatched RPC ID",
            |t| t["responses"][2]["response"]["id"] = json!(99),
            "identity mismatch",
        ),
        (
            "missing RPC ID",
            |t| {
                t["responses"][2]["request"]
                    .as_object_mut()
                    .unwrap()
                    .remove("id");
            },
            "identity mismatch",
        ),
        (
            "server error",
            |t| t["responses"][2]["response"]["error"] = json!({"message":"denied"}),
            "API returned an error",
        ),
        (
            "wrong method",
            |t| t["responses"][2]["request"]["method"] = json!("thread/start"),
            "Unsupported Codex API method",
        ),
        (
            "wrong worktree",
            |t| t["responses"][0]["response"]["result"]["thread"]["cwd"] = json!("/other"),
            "different worktree",
        ),
        (
            "local substitute",
            |t| t["responses"][0]["response"]["result"]["thread"]["source"] = json!("cli"),
            "not a current cloud agent",
        ),
        (
            "missing environment",
            |t| t["responses"][0]["response"]["result"]["thread"]["environments"] = json!([]),
            "lacks cloud environment",
        ),
        (
            "missing page",
            |t| t["responses"][2]["response"]["result"]["nextCursor"] = json!("missing"),
            "Incomplete Codex API pagination",
        ),
        (
            "cycle",
            |t| {
                let mut p = t["responses"][2].clone();
                t["responses"][2]["response"]["result"]["nextCursor"] = json!("cycle");
                p["request"]["params"]["cursor"] = json!("cycle");
                p["response"]["result"]["nextCursor"] = json!("cycle");
                t["responses"].as_array_mut().unwrap().push(p);
            },
            "Cyclic",
        ),
        (
            "orphan",
            |t| {
                let mut p = t["responses"][2].clone();
                p["request"]["params"]["cursor"] = json!("orphan");
                t["responses"].as_array_mut().unwrap().push(p);
            },
            "Orphan",
        ),
        (
            "conflicting replay",
            |t| {
                let mut p = t["responses"][2].clone();
                p["response"]["result"]["data"][0]["item"]["content"][0]["text"] =
                    json!("conflicting");
                t["responses"].as_array_mut().unwrap().push(p);
            },
            "Conflicting",
        ),
        (
            "duplicate item",
            |t| {
                let item = t["responses"][2]["response"]["result"]["data"][0].clone();
                t["responses"][2]["response"]["result"]["data"]
                    .as_array_mut()
                    .unwrap()
                    .push(item);
            },
            "Duplicate Codex API item",
        ),
        (
            "unknown turn",
            |t| t["responses"][2]["response"]["result"]["data"][0]["turnId"] = json!("foreign"),
            "unknown turn",
        ),
        (
            "filtered history",
            |t| t["responses"][2]["request"]["params"]["after"] = json!("filtered"),
            "not a full-thread export",
        ),
        (
            "invalid direction",
            |t| t["responses"][2]["request"]["params"]["sortDirection"] = json!(3),
            "sort direction",
        ),
        (
            "missing turn pages",
            |t| {
                t["responses"].as_array_mut().unwrap().remove(1);
            },
            "Missing complete",
        ),
        (
            "missing cursor declaration",
            |t| {
                t["responses"][2]["response"]["result"]
                    .as_object_mut()
                    .unwrap()
                    .remove("nextCursor");
            },
            "lacks nextCursor",
        ),
        (
            "unknown call status",
            |t| {
                t["responses"][2]["response"]["result"]["data"][1]["item"]["status"] =
                    json!("finished-ish")
            },
            "Unknown Codex API call status",
        ),
        (
            "missing exit status",
            |t| {
                t["responses"][2]["response"]["result"]["data"][1]["item"]
                    .as_object_mut()
                    .unwrap()
                    .remove("exitCode");
            },
            "lacks exitCode",
        ),
        (
            "foreign supplemental turn",
            |t| {
                let mut p = t["responses"][2].clone();
                p["request"]["params"]["turnId"] = json!("turn-1");
                p["response"]["result"]["data"][0]["turnId"] = json!("foreign");
                t["responses"].as_array_mut().unwrap().push(p);
            },
            "different turn identity",
        ),
    ];
    for (name, mutate, message) in cases {
        let mut source = trace();
        mutate(&mut source);
        let error = check(&source).expect_err(name);
        ensure!(format!("{error:#}").contains(message), "{name}: {error:#}");
    }
    Ok(())
}

#[test]
fn identical_replays_are_idempotent_and_descending_history_is_ordered() -> Result<()> {
    let mut source = trace();
    let page = source["responses"][2].clone();
    source["responses"].as_array_mut().unwrap().push(page);
    let (parsed, summary) = codex_api::parse(ID, Path::new("/workspace"), &normalized(&source)?)?;
    ensure!(summary["item_count"] == 2);
    ensure!(
        parsed
            .observations
            .iter()
            .filter(|v| v["kind"] == "tool.started")
            .count()
            == 1
    );
    source["responses"].as_array_mut().unwrap().pop();
    source["responses"][2]["request"]["params"]["sortDirection"] = json!("desc");
    source["responses"][2]["response"]["result"]["data"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let (ordered, _) = codex_api::parse(ID, Path::new("/workspace"), &normalized(&source)?)?;
    ensure!(parsed.canonical == ordered.canonical);
    Ok(())
}

#[test]
fn arbitrary_mcp_payload_keys_do_not_bypass_redaction() -> Result<()> {
    let mut source = trace();
    let secret = "DB_PASSWORD=synthetic-api-secret-123456";
    source["responses"][2]["response"]["result"]["data"].as_array_mut().unwrap().push(json!({
        "turnId":"turn-1","item":{"id":"mcp-1","type":"mcpToolCall","server":"ghp_000000000000000000000000000000000000","tool":"inspect",
        "arguments":{"id":secret,"name":secret,"cwd":secret},"result":{"name":secret,"path":secret},"error":null,"status":"completed"}}));
    let mut redactor = redact::Redactor::new(&json!({}))?;
    let (full, parsed, summary) = codex_api::checkpoint(
        ID,
        Path::new("/workspace"),
        &normalized(&source)?,
        &mut redactor,
    )?;
    for text in [
        full,
        serde_json::to_string(&parsed.canonical)?,
        serde_json::to_string(&parsed.observations)?,
        serde_json::to_string(&summary)?,
    ] {
        ensure!(!text.contains("synthetic-api-secret"));
        ensure!(!text.contains("ghp_000000000000000000000000000000000000"));
    }
    ensure!(
        summary["environments"][0]["environmentId"]
            == "ccarenv_b64_U3ludGhldGljQ2xvdWRSdW50aW1lMTIzNDU"
    );
    Ok(())
}

#[test]
fn sensitive_pending_call_id_is_private_and_correlatable() -> Result<()> {
    let mut source = trace();
    let secret = "ghp_000000000000000000000000000000000000";
    source["responses"][2]["response"]["result"]["data"][1]["item"]["id"] = json!(secret);
    source["responses"][2]["response"]["result"]["data"][1]["item"]["status"] = json!("inProgress");
    let (full, parsed, _) = codex_api::checkpoint(
        ID,
        Path::new("/workspace"),
        &normalized(&source)?,
        &mut redact::Redactor::new(&json!({}))?,
    )?;
    let expected = format!("api-redacted-{}", capture::hash(secret));
    ensure!(parsed.pending_calls.contains(&expected));
    ensure!(
        parsed
            .canonical
            .iter()
            .any(|e| e["data"]["toolCallId"] == expected)
    );
    for text in [
        full,
        serde_json::to_string(&parsed.canonical)?,
        serde_json::to_string(&parsed.observations)?,
        serde_json::to_string(&parsed.pending_calls)?,
    ] {
        ensure!(!text.contains(secret));
    }
    Ok(())
}

#[test]
fn api_import_preserves_same_uuid_local_state_and_rejects_before_mutation() -> Result<()> {
    let f = support::Fixture::new()?;
    f.g(&[
        "remote",
        "add",
        "origin",
        "https://github.com/madsbolaris/trailvault-capture-test.git",
    ])?;
    support::invoke(
        &f.repo,
        &["capture", "codex", "session-start", "-"],
        &f.env,
        Some(&json!({"sessionId":ID})),
    )?;
    let repo = git::Repo::discover(&f.repo)?;
    let before = serde_json::to_value(capture::sessions(&repo)?)?;
    let source = trace();
    let path = f.temp.path().join("api.json");
    support::put(&path, serde_json::to_vec(&source)?)?;
    let payload = json!({"native_session_id":ID,"transcript_path":path,"provenance":{
        "remote_session_id":ID,"repository":"madsbolaris/trailvault-capture-test","source_commit":f.g(&["rev-parse","HEAD"])?,
        "source_branch":"cloud/source","transcript_cwd":"/workspace","collected_at":source["collectedAt"],
        "collection":"codex-app-server","completeness":"available-persisted-history","identity_basis":"collector_asserted"}});
    let mut bad = source.clone();
    bad["responses"][2]["response"]["id"] = json!(99);
    support::put(&path, serde_json::to_vec(&bad)?)?;
    ensure!(capture::hosted_checkpoint(&repo, &payload).is_err());
    ensure!(
        f.g(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/trailvault/checkpoints/"
        ])?
        .is_empty()
    );
    ensure!(serde_json::to_value(capture::sessions(&repo)?)? == before);
    support::put(&path, serde_json::to_vec(&source)?)?;
    capture::hosted_checkpoint(&repo, &payload)?;
    ensure!(serde_json::to_value(capture::sessions(&repo)?)? == before);
    Ok(())
}
