use crate::{native, redact};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const COLLECTION: &str = "codex-app-server";
pub const COMPLETENESS: &str = "available-persisted-history";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Trace {
    schema: String,
    thread_id: String,
    collected_at: String,
    collection: String,
    responses: Vec<Value>,
}

pub fn normalize(raw: &str, id: &str, collected_at: &str) -> Result<String> {
    let trace: Trace = serde_json::from_str(raw).context("Invalid Codex API capture")?;
    ensure!(
        trace.schema == "trailvault.codex-app-server-observation.v1",
        "Unsupported Codex API capture schema"
    );
    ensure!(
        trace.thread_id == id,
        "Codex API capture has a different session identity"
    );
    ensure!(
        trace.collected_at == collected_at,
        "Codex API collection timestamp mismatch"
    );
    ensure!(
        matches!(
            trace.collection.as_str(),
            "passive_provider_client_rpc" | "authorized_app_server_rpc"
        ),
        "Unsupported Codex API transport"
    );
    let mut records = vec![json!({"type":"codex.app_server.capture","id":id,
        "collectedAt":trace.collected_at,"collection":trace.collection})];
    for mut exchange in trace.responses {
        let object = exchange
            .as_object_mut()
            .context("Codex API exchange must be an object")?;
        ensure!(
            object
                .keys()
                .all(|key| ["request", "response"].contains(&key.as_str())),
            "Unknown Codex API exchange field"
        );
        object.insert("type".into(), json!("codex.app_server.rpc"));
        records.push(exchange);
    }
    redact::jsonl(&records)
}

fn identifier(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 512
                && !s.chars().any(|c| c.is_control() || c.is_whitespace())
        })
        .context("Missing or invalid Codex API identity")
}

fn cursor(value: &Value) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(
        value
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .context("Invalid Codex API cursor")?
            .into(),
    ))
}

fn environments(thread: &Value) -> Result<Value> {
    let rows = thread["environments"]
        .as_array()
        .context("Codex API thread lacks cloud environment identity")?;
    ensure!(
        !rows.is_empty(),
        "Codex API thread lacks cloud environment identity"
    );
    let mut identities = vec![];
    for row in rows {
        let id = identifier(&row["environmentId"])?;
        ensure!(
            id.strip_prefix("ccarenv_b64_")
                .is_some_and(|s| !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))),
            "Invalid Codex API cloud environment identity"
        );
        let mut identity = json!({"environmentId":id});
        if !row["environmentConfigId"].is_null() {
            let config = identifier(&row["environmentConfigId"])?;
            let (owner, suffix) = config
                .split_once('~')
                .context("Invalid Codex API cloud configuration identity")?;
            uuid::Uuid::parse_str(owner)
                .context("Invalid Codex API configuration owner identity")?;
            ensure!(
                ["asenvcfg_", "cecfg_"].iter().any(|prefix| suffix
                    .strip_prefix(prefix)
                    .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))),
                "Invalid Codex API cloud configuration identity"
            );
            identity["environmentConfigId"] = json!(config);
        }
        identities.push(identity);
    }
    Ok(json!(identities))
}

fn pages(records: &[Value], method: &str, turn: Option<&str>) -> Result<(Vec<Value>, usize)> {
    let mut groups = BTreeMap::new();
    for record in records.iter().filter(|r| {
        r["request"]["method"] == method && r["request"]["params"]["turnId"].as_str() == turn
    }) {
        let params = &record["request"]["params"];
        ensure!(
            params
                .as_object()
                .context("Missing Codex API parameters")?
                .keys()
                .all(|k| [
                    "threadId",
                    "turnId",
                    "cursor",
                    "limit",
                    "sortDirection",
                    "itemsView"
                ]
                .contains(&k.as_str())),
            "Filtered Codex API history is not a full-thread export"
        );
        let sort = if params["sortDirection"].is_null() {
            "desc"
        } else {
            params["sortDirection"]
                .as_str()
                .context("Invalid Codex API sort direction")?
        };
        ensure!(
            matches!(sort, "asc" | "desc"),
            "Invalid Codex API sort direction"
        );
        let limit = params
            .get("limit")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n > 0)
                    .context("Invalid Codex API page limit")
            })
            .transpose()?;
        let result = &record["response"]["result"];
        let data = result["data"]
            .as_array()
            .context("Codex API page lacks data")?;
        ensure!(
            result.get("nextCursor").is_some(),
            "Codex API page lacks nextCursor"
        );
        let page = (data.clone(), cursor(&result["nextCursor"])?);
        let key = cursor(&params["cursor"])?;
        if let Some(previous) = groups
            .entry((sort, limit))
            .or_insert_with(BTreeMap::new)
            .insert(key, page.clone())
        {
            ensure!(
                previous == page,
                "Conflicting Codex API pages; collect a coherent snapshot"
            );
        }
    }
    ensure!(
        !groups.is_empty(),
        "Missing complete Codex API {method} history"
    );
    let mut selected = None;
    let mut previews = 0;
    for ((direction, _), pages) in groups {
        let mut next = None;
        let mut visited = BTreeSet::new();
        let mut rows = vec![];
        let mut complete = false;
        loop {
            ensure!(visited.insert(next.clone()), "Cyclic Codex API pagination");
            let Some((data, cursor)) = pages.get(&next) else {
                break;
            };
            rows.extend(data.iter().cloned());
            next = cursor.clone();
            if next.is_none() {
                complete = true;
                break;
            }
        }
        if !complete {
            ensure!(
                visited.len() == pages.len() + 1,
                "Orphan Codex API history pages"
            );
            previews += 1;
            continue;
        }
        ensure!(
            visited.len() == pages.len(),
            "Orphan Codex API history pages"
        );
        if direction == "desc" {
            rows.reverse();
        }
        if let Some(previous) = &selected {
            ensure!(
                *previous == rows,
                "Conflicting complete Codex API histories"
            );
        } else {
            selected = Some(rows);
        }
    }
    Ok((
        selected.context("Incomplete Codex API pagination")?,
        previews,
    ))
}

pub fn parse(id: &str, cwd: &Path, raw: &str) -> Result<(native::Parsed, Value)> {
    let records = redact::parse(raw)?;
    ensure!(
        records
            .first()
            .is_some_and(|r| r["type"] == "codex.app_server.capture" && r["id"] == id),
        "Codex API capture has a different session identity"
    );
    let rpc = &records[1..];
    let mut metadata = None;
    for record in rpc {
        ensure!(
            record["type"] == "codex.app_server.rpc",
            "Unexpected Codex API record"
        );
        let request = &record["request"];
        ensure!(
            request["params"]["threadId"] == id,
            "Codex API request targets a different session identity"
        );
        let request_id = &request["id"];
        ensure!(
            (request_id.is_string() || request_id.is_i64() || request_id.is_u64())
                && request_id == &record["response"]["id"],
            "Codex API request/response identity mismatch"
        );
        ensure!(
            record["response"].get("error").is_none(),
            "Codex API returned an error: {}",
            record["response"]["error"]
        );
        ensure!(
            record["response"]["result"].is_object(),
            "Codex API response lacks result"
        );
        match request["method"].as_str() {
            Some("thread/read") => {
                let thread = &record["response"]["result"]["thread"];
                ensure!(
                    thread["id"] == id,
                    "Codex API response has a different session identity"
                );
                ensure!(
                    thread["cwd"].as_str().is_some_and(|p| Path::new(p) == cwd),
                    "Codex API transcript belongs to a different worktree"
                );
                ensure!(
                    thread["source"]["custom"] == "codex-cloud-agent",
                    "Codex API thread is not a current cloud agent"
                );
                environments(thread)?;
                metadata = Some(thread.clone());
            }
            Some("thread/turns/list" | "thread/items/list") => {}
            _ => anyhow::bail!("Unsupported Codex API method"),
        }
    }
    let metadata = metadata.context("Codex API capture lacks thread/read metadata")?;
    let (turns, mut preview_chains) = pages(rpc, "thread/turns/list", None)?;
    let mut turn_ids = BTreeSet::new();
    for turn in &turns {
        ensure!(
            turn_ids.insert(identifier(&turn["id"])?.to_owned()),
            "Duplicate Codex API turn"
        );
    }
    for record in rpc {
        if let Some(turn) = record["request"]["params"]
            .get("turnId")
            .filter(|v| !v.is_null())
        {
            ensure!(
                turn_ids.contains(identifier(turn)?),
                "Codex API request targets an unknown turn"
            );
        }
    }
    let whole_thread = rpc.iter().any(|r| {
        r["request"]["method"] == "thread/items/list" && r["request"]["params"]["turnId"].is_null()
    });
    let items = if whole_thread {
        let (rows, previews) = pages(rpc, "thread/items/list", None)?;
        preview_chains += previews;
        rows
    } else {
        let mut rows = vec![];
        for turn in &turns {
            let (page, previews) = pages(rpc, "thread/items/list", Some(identifier(&turn["id"])?))?;
            rows.extend(page);
            preview_chains += previews;
        }
        rows
    };
    let mut item_ids = BTreeSet::new();
    for row in &items {
        ensure!(
            turn_ids.contains(identifier(&row["turnId"])?),
            "Codex API item belongs to an unknown turn"
        );
        ensure!(
            item_ids.insert(identifier(&row["item"]["id"])?.to_owned()),
            "Duplicate Codex API item"
        );
        ensure!(row["item"]["type"].is_string(), "Codex API item lacks type");
    }
    for record in rpc
        .iter()
        .filter(|r| r["request"]["method"] == "thread/items/list")
    {
        let scope = record["request"]["params"]["turnId"].as_str();
        for row in record["response"]["result"]["data"]
            .as_array()
            .context("Codex API page lacks data")?
        {
            let turn = identifier(&row["turnId"])?;
            ensure!(
                turn_ids.contains(turn) && scope.is_none_or(|s| s == turn),
                "Supplemental Codex API item has a different turn identity"
            );
            ensure!(
                item_ids.contains(identifier(&row["item"]["id"])?),
                "Supplemental Codex API item is absent from full history"
            );
        }
    }
    let mut parsed = native::Parsed {
        canonical: vec![],
        observations: vec![],
        references: vec![],
        pending_calls: BTreeSet::new(),
    };
    for (index, row) in items.iter().enumerate() {
        let line = index + 1;
        let item = &row["item"];
        match item["type"].as_str() {
            Some("userMessage") => {
                for block in item["content"]
                    .as_array()
                    .context("Codex API user message lacks content")?
                {
                    if block["type"] == "text" {
                        native::message(
                            &mut parsed,
                            "user",
                            block["text"]
                                .as_str()
                                .context("Invalid Codex API user text")?,
                            line,
                        )?;
                    }
                }
            }
            Some("agentMessage") => native::message(
                &mut parsed,
                "assistant",
                item["text"]
                    .as_str()
                    .context("Invalid Codex API agent text")?,
                line,
            )?,
            Some("commandExecution" | "mcpToolCall") => {
                let command = item["type"] == "commandExecution";
                let name = if command {
                    json!("commandExecution")
                } else {
                    json!(format!(
                        "{}.{}",
                        identifier(&item["server"])?,
                        identifier(&item["tool"])?
                    ))
                };
                let args = if command {
                    json!({"command":item["command"].as_str().context("Codex API command lacks command")?,"cwd":item["cwd"]})
                } else {
                    item.get("arguments")
                        .context("Codex API MCP call lacks arguments")?
                        .clone()
                };
                native::tool_start(&mut parsed, &item["id"], &name, &args, line)?;
                let status = item["status"]
                    .as_str()
                    .context("Codex API call lacks status")?;
                match status {
                    "inProgress" | "running" => {}
                    "completed" | "failed" | "declined" | "cancelled" | "interrupted" => {
                        let failed = status != "completed"
                            || item.get("error").is_some_and(|e| !e.is_null())
                            || item["result"]["isError"] == true
                            || (command
                                && item["exitCode"]
                                    .as_i64()
                                    .context("Terminal Codex API command lacks exitCode")?
                                    != 0);
                        let output = if command {
                            json!({"output":item["aggregatedOutput"],"exit_code":item["exitCode"],"status":status})
                        } else {
                            json!({"result":item["result"],"error":item["error"],"status":status})
                        };
                        native::tool_result(&mut parsed, &item["id"], &output, failed, line)?;
                    }
                    _ => anyhow::bail!("Unknown Codex API call status: {status}"),
                }
            }
            _ => {}
        }
        parsed.observations.push(json!({"kind":"native.api_item","native_line":line,
            "turn_id":row["turnId"],"item_id":item["id"],"native_type":item["type"],
            "started_at_ms":row["startedAtMs"],"completed_at_ms":row["completedAtMs"],"event":row}));
    }
    let summary = json!({"thread_id":id,"turn_count":turns.len(),"item_count":items.len(),
        "pagination":"available_history_fully_paged","pagination_scope":if whole_thread {"whole_thread"} else {"per_turn"},
        "incomplete_preview_chains":preview_chains,"original_history_complete":false,
        "basis":"persisted item projections, not streamed lifecycle hooks; provider compaction/output limits may apply",
        "provider_reported_status":metadata["status"],"environments":environments(&metadata)?});
    Ok((parsed, summary))
}

fn redact_value(value: &Value, redactor: &mut redact::Redactor) -> Value {
    match value {
        Value::String(s) => json!(redactor.text(s)),
        Value::Array(values) => {
            Value::Array(values.iter().map(|v| redact_value(v, redactor)).collect())
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(k, v)| (k.clone(), redact_value(v, redactor)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn private_call_id(id: &str, redactor: &mut redact::Redactor) -> String {
    if redactor.text(id) == id {
        id.into()
    } else {
        format!("api-redacted-{}", crate::capture::hash(id))
    }
}

pub fn checkpoint(
    id: &str,
    cwd: &Path,
    raw: &str,
    redactor: &mut redact::Redactor,
) -> Result<(String, native::Parsed, Value)> {
    let (mut parsed, summary) = parse(id, cwd, raw)?;
    let full = redact::jsonl(
        &redact::parse(raw)?
            .iter()
            .map(|v| redact_value(v, redactor))
            .collect::<Vec<_>>(),
    )?;
    for event in &mut parsed.canonical {
        let kind = event["type"].clone();
        let call_id = event["data"]["toolCallId"]
            .as_str()
            .map(|id| private_call_id(id, redactor));
        *event = redact_value(event, redactor);
        event["type"] = kind;
        if let Some(id) = call_id {
            event["data"]["toolCallId"] = json!(id);
        }
    }
    parsed.observations = parsed
        .observations
        .iter()
        .map(|v| {
            let mut value = redact_value(v, redactor);
            if let Some(id) = v["call_id"].as_str() {
                value["call_id"] = json!(private_call_id(id, redactor));
            }
            value
        })
        .collect();
    parsed.pending_calls = parsed
        .pending_calls
        .iter()
        .map(|id| private_call_id(id, redactor))
        .collect();
    parsed.references = parsed
        .references
        .iter()
        .map(|v| redact_value(v, redactor))
        .collect();
    let mut summary_redacted = redact_value(&summary, redactor);
    summary_redacted["environments"] = summary["environments"].clone();
    Ok((full, parsed, summary_redacted))
}
