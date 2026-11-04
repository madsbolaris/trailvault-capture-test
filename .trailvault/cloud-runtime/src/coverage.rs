use crate::{capture, native, redact};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    CopilotLocal,
    CopilotCloud,
    ClaudeLocal,
    ClaudeCloud,
    CodexLocal,
    CodexCloud,
}

impl Source {
    pub fn for_capture(context: Option<&native::ContextSnapshot>, cloud: bool) -> Self {
        match context {
            None if cloud => Self::CopilotCloud,
            None => Self::CopilotLocal,
            Some(c) => match (c.provider, c.hosted.is_some()) {
                (native::Provider::Claude, false) => Self::ClaudeLocal,
                (native::Provider::Claude, true) => Self::ClaudeCloud,
                (native::Provider::Codex, false) => Self::CodexLocal,
                (native::Provider::Codex, true) => Self::CodexCloud,
            },
        }
    }

    fn basis(self) -> &'static str {
        match self {
            Self::CopilotLocal | Self::CopilotCloud => {
                "publication_configuration_not_runtime_attestation"
            }
            _ => "collector_context_not_runtime_attestation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    UserMessages,
    AssistantMessages,
    ToolCalls,
    ToolResults,
    EventIds,
    Timestamps,
    ParentLinks,
    SessionIdentity,
    Model,
    Usage,
    Cost,
    Reasoning,
    InstructionsMemory,
    SkillsTools,
    AgentCoordination,
    AttachmentReferences,
    AttachmentPayloads,
    CompactionEvents,
    NativeRecords,
}

pub const FIELDS: [Field; 19] = [
    Field::UserMessages,
    Field::AssistantMessages,
    Field::ToolCalls,
    Field::ToolResults,
    Field::EventIds,
    Field::Timestamps,
    Field::ParentLinks,
    Field::SessionIdentity,
    Field::Model,
    Field::Usage,
    Field::Cost,
    Field::Reasoning,
    Field::InstructionsMemory,
    Field::SkillsTools,
    Field::AgentCoordination,
    Field::AttachmentReferences,
    Field::AttachmentPayloads,
    Field::CompactionEvents,
    Field::NativeRecords,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Captured,
    Unavailable,
    Redacted,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Availability {
    pub status: Status,
    pub observed_values: usize,
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldCoverage {
    pub native: Availability,
    pub standardized: Availability,
    pub native_values_changed_by_redaction: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub status: Status,
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub version: u8,
    pub source: Source,
    pub source_basis: String,
    pub native_sha256: String,
    pub standardized_sha256: String,
    pub standardized_scope: String,
    pub count_unit: String,
    pub fields: BTreeMap<Field, FieldCoverage>,
    pub guarantees: BTreeMap<String, Claim>,
}

#[derive(Default)]
struct Counts {
    native: usize,
    changed: usize,
    standardized: usize,
}
type Measurements = BTreeMap<Field, Counts>;

struct Observer<'a> {
    original: &'a Value,
    published: &'a Value,
    counts: &'a mut Measurements,
}
impl Observer<'_> {
    fn at(&mut self, field: Field, path: &str) {
        let Some(value) = self.original.pointer(path) else {
            return;
        };
        if value.is_null() && field != Field::ParentLinks {
            return;
        }
        if matches!(
            field,
            Field::Model | Field::Usage | Field::Cost | Field::Reasoning
        ) && (value.as_str().is_some_and(str::is_empty)
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty))
        {
            return;
        }
        let count = self.counts.entry(field).or_default();
        count.native += 1;
        count.changed += usize::from(Some(value) != self.published.pointer(path));
    }

    fn content_blocks(&mut self, path: &str) {
        let raw = self.original;
        if let Some(blocks) = raw.pointer(path).and_then(Value::as_array) {
            for (index, block) in blocks.iter().enumerate() {
                let path = format!("{path}/{index}");
                match block["type"].as_str() {
                    Some("text") => {}
                    Some("tool_use") => self.at(Field::ToolCalls, &path),
                    Some("tool_result") => self.at(Field::ToolResults, &path),
                    Some("thinking") => self.at(Field::Reasoning, &format!("{path}/thinking")),
                    Some("image" | "document") => {
                        self.at(Field::AttachmentReferences, &format!("{path}/source/url"));
                        self.at(Field::AttachmentPayloads, &format!("{path}/source/data"));
                    }
                    _ => {}
                }
            }
        }
    }

    fn native_event(&mut self, source: Source) {
        let raw = self.original;
        self.at(Field::NativeRecords, "");
        self.at(Field::Timestamps, "/timestamp");
        match source {
            Source::CopilotLocal | Source::CopilotCloud => {
                self.at(Field::EventIds, "/id");
                self.at(Field::ParentLinks, "/parentId");
                for (field, path) in [
                    (Field::Model, "/data/model"),
                    (Field::Model, "/data/newModel"),
                    (Field::Usage, "/data/usage"),
                    (Field::Usage, "/data/modelMetrics"),
                    (Field::Usage, "/data/outputTokens"),
                    (Field::Usage, "/data/inputTokens"),
                    (Field::Cost, "/data/cost"),
                    (Field::Cost, "/data/usage/cost"),
                ] {
                    self.at(field, path);
                }
                match raw["type"].as_str() {
                    Some("session.start" | "session.resume") => {
                        self.at(Field::SessionIdentity, "/data/sessionId")
                    }
                    Some("user.message" | "assistant.message") => {
                        self.at(
                            if raw["type"] == "user.message" {
                                Field::UserMessages
                            } else {
                                Field::AssistantMessages
                            },
                            "/data/content",
                        );
                        if let Some(requests) = raw["data"]["toolRequests"].as_array() {
                            for index in 0..requests.len() {
                                self.at(Field::ToolCalls, &format!("/data/toolRequests/{index}"));
                            }
                        }
                        if let Some(attachments) = raw["data"]["attachments"].as_array() {
                            for index in 0..attachments.len() {
                                for key in ["path", "url"] {
                                    self.at(
                                        Field::AttachmentReferences,
                                        &format!("/data/attachments/{index}/{key}"),
                                    );
                                }
                                for key in ["data", "bytes"] {
                                    self.at(
                                        Field::AttachmentPayloads,
                                        &format!("/data/attachments/{index}/{key}"),
                                    );
                                }
                            }
                        }
                    }
                    Some("tool.execution_start") => self.at(Field::ToolCalls, "/data"),
                    Some("tool.execution_complete") => self.at(Field::ToolResults, "/data"),
                    Some("assistant.reasoning") => self.at(Field::Reasoning, "/data/content"),
                    Some("system.message" | "developer.message") => {
                        self.at(Field::InstructionsMemory, "/data/content")
                    }
                    Some("skill.activated" | "tools.catalog") => {
                        self.at(Field::SkillsTools, "/data")
                    }
                    Some("subagent.started" | "subagent.completed") => {
                        self.at(Field::AgentCoordination, "")
                    }
                    Some("session.compaction_start" | "session.compaction_complete") => {
                        self.at(Field::CompactionEvents, "")
                    }
                    _ => {}
                }
            }
            Source::ClaudeLocal | Source::ClaudeCloud => {
                self.at(Field::EventIds, "/uuid");
                self.at(Field::ParentLinks, "/parentUuid");
                self.at(Field::SessionIdentity, "/sessionId");
                self.at(Field::Model, "/message/model");
                self.at(Field::Usage, "/message/usage");
                self.at(Field::Cost, "/total_cost_usd");
                match raw["type"].as_str() {
                    Some("user" | "assistant") => {
                        let field = if raw["type"] == "user" {
                            Field::UserMessages
                        } else {
                            Field::AssistantMessages
                        };
                        if raw["message"]["content"].is_string() {
                            self.at(field, "/message/content");
                        }
                        if let Some(blocks) = raw["message"]["content"].as_array() {
                            for (index, block) in blocks.iter().enumerate() {
                                if block["type"] == "text" {
                                    self.at(field, &format!("/message/content/{index}/text"));
                                }
                            }
                        }
                        self.content_blocks("/message/content");
                    }
                    Some("system") => {
                        self.at(Field::InstructionsMemory, "/content");
                        if raw["subtype"] == "compact_boundary" {
                            self.at(Field::CompactionEvents, "");
                        }
                    }
                    _ => {}
                }
            }
            Source::CodexLocal => {
                if raw["type"] == "session_meta" {
                    self.at(Field::SessionIdentity, "/payload/id");
                }
                if raw["type"] == "turn_context" {
                    self.at(Field::Model, "/payload/model");
                }
                if raw["type"] == "event_msg" && raw["payload"]["type"] == "token_count" {
                    self.at(Field::Usage, "/payload/info");
                }
                if raw["type"] == "compacted" {
                    self.at(Field::CompactionEvents, "");
                }
                if raw["type"] == "response_item" {
                    self.at(Field::EventIds, "/payload/id");
                    match raw["payload"]["type"].as_str() {
                        Some("message") => {
                            if let Some(blocks) = raw["payload"]["content"].as_array() {
                                for (index, _) in blocks.iter().enumerate() {
                                    let field = match raw["payload"]["role"].as_str() {
                                        Some("user") => Field::UserMessages,
                                        Some("assistant") => Field::AssistantMessages,
                                        Some("system" | "developer") => Field::InstructionsMemory,
                                        _ => continue,
                                    };
                                    self.at(field, &format!("/payload/content/{index}/text"));
                                    self.at(
                                        Field::AttachmentReferences,
                                        &format!("/payload/content/{index}/image_url"),
                                    );
                                }
                            }
                        }
                        Some("function_call" | "custom_tool_call") => {
                            self.at(Field::ToolCalls, "/payload")
                        }
                        Some("function_call_output" | "custom_tool_call_output") => {
                            self.at(Field::ToolResults, "/payload")
                        }
                        Some("reasoning") => self.at(Field::Reasoning, "/payload/summary"),
                        _ => {}
                    }
                }
            }
            Source::CodexCloud => match raw["request"]["method"].as_str() {
                Some("thread/read") => {
                    self.at(Field::SessionIdentity, "/response/result/thread/id")
                }
                Some("thread/turns/list") => {
                    if let Some(turns) = raw["response"]["result"]["data"].as_array() {
                        for index in 0..turns.len() {
                            for key in ["usage", "tokenUsage"] {
                                self.at(
                                    Field::Usage,
                                    &format!("/response/result/data/{index}/{key}"),
                                );
                            }
                        }
                    }
                }
                Some("thread/items/list") => {
                    if let Some(items) = raw["response"]["result"]["data"].as_array() {
                        for (index, row) in items.iter().enumerate() {
                            let path = format!("/response/result/data/{index}");
                            self.at(Field::EventIds, &format!("{path}/item/id"));
                            self.at(Field::Timestamps, &format!("{path}/startedAtMs"));
                            self.at(Field::Timestamps, &format!("{path}/completedAtMs"));
                            let item = format!("{path}/item");
                            match row["item"]["type"].as_str() {
                                Some("userMessage") => {
                                    if let Some(blocks) = row["item"]["content"].as_array() {
                                        for index in 0..blocks.len() {
                                            self.at(
                                                Field::UserMessages,
                                                &format!("{item}/content/{index}/text"),
                                            );
                                        }
                                    }
                                }
                                Some("agentMessage") => {
                                    self.at(Field::AssistantMessages, &format!("{item}/text"))
                                }
                                Some("commandExecution" | "mcpToolCall") => {
                                    self.at(Field::ToolCalls, &item);
                                    if matches!(
                                        row["item"]["status"].as_str(),
                                        Some(
                                            "completed"
                                                | "failed"
                                                | "declined"
                                                | "cancelled"
                                                | "interrupted"
                                        )
                                    ) {
                                        self.at(Field::ToolResults, &item);
                                    }
                                }
                                Some("reasoning") => {
                                    self.at(Field::Reasoning, &format!("{item}/summary"));
                                    self.at(Field::Reasoning, &format!("{item}/content"));
                                }
                                Some("contextCompaction") => {
                                    self.at(Field::CompactionEvents, &item)
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

fn measurements(
    source: Source,
    original: &str,
    published: &str,
    compact: &str,
) -> Result<Measurements> {
    let original = redact::parse(original)?;
    let published = redact::parse(published)?;
    ensure!(
        original.len() == published.len(),
        "Coverage requires aligned original and published records"
    );
    let mut counts: Measurements = FIELDS
        .into_iter()
        .map(|field| (field, Counts::default()))
        .collect();
    for (original, published) in original.iter().zip(&published) {
        if source == Source::CodexCloud && original["type"] == "codex.app_server.capture" {
            continue;
        }
        Observer {
            original,
            published,
            counts: &mut counts,
        }
        .native_event(source);
    }
    for row in redact::parse(compact)?.iter().skip(1) {
        let field = match row["type"].as_str() {
            Some("message") if !row["content"].is_null() => match row["role"].as_str() {
                Some("user") => Some(Field::UserMessages),
                Some("assistant") => Some(Field::AssistantMessages),
                _ => None,
            },
            Some("tool.execution_start") if !row["data"].is_null() => Some(Field::ToolCalls),
            Some("tool.execution_complete") if !row["data"].is_null() => Some(Field::ToolResults),
            Some("context") if !row["content"].is_null() => match row["kind"].as_str() {
                Some("assistant.reasoning") => Some(Field::Reasoning),
                Some("system.message" | "developer.message") => Some(Field::InstructionsMemory),
                Some("skill.activated" | "tools.catalog") => Some(Field::SkillsTools),
                _ => None,
            },
            _ => None,
        };
        if let Some(field) = field {
            counts.entry(field).or_default().standardized += 1;
        }
        if row["classification"]["provenance"] == "agent_coordination" {
            counts
                .entry(Field::AgentCoordination)
                .or_default()
                .standardized += 1;
        }
        for (field, key) in [
            (Field::EventIds, "id"),
            (Field::Timestamps, "timestamp"),
            (Field::ParentLinks, "parentId"),
        ] {
            if row.get(key).is_some() {
                counts.entry(field).or_default().standardized += 1;
            }
        }
    }
    Ok(counts)
}

fn availability(count: usize, changed: usize, native_count: Option<usize>) -> Availability {
    let (status, basis) = if count > 0 && changed > 0 {
        (
            Status::Redacted,
            "source_field_redacted; projection_may_reduce_further",
        )
    } else if count > 0 {
        (
            Status::Captured,
            "observed_in_this_snapshot; completeness_not_established",
        )
    } else if native_count.is_some_and(|count| count > 0) {
        (Status::Unavailable, "not_projected_into_compact_schema")
    } else {
        (
            Status::Unverified,
            "not_observed; absence_does_not_establish_provider_unavailability",
        )
    };
    Availability {
        status,
        observed_values: count,
        basis: basis.into(),
    }
}

fn guarantees(source: Source) -> BTreeMap<String, Claim> {
    [
        (
            "original_history_complete",
            Status::Unverified,
            "provider_compaction_or_output_limits_may_apply",
        ),
        (
            "live_presence",
            Status::Unavailable,
            "snapshot_has_no_renewable_presence_lease",
        ),
        (
            "runtime_termination",
            Status::Unverified,
            "client_exit_or_completed_records_do_not_prove_runtime_termination",
        ),
        (
            "background_jobs_complete",
            Status::Unverified,
            "background_processes_not_independently_observed",
        ),
        (
            "child_transcripts_complete",
            Status::Unverified,
            "coordination_or_task_references_do_not_prove_child_transcript_collection",
        ),
        (
            "attachments_complete",
            Status::Unverified,
            "inline_payloads_or_references_do_not_prove_all_attachment_bytes_collected",
        ),
        (
            "hidden_model_state",
            Status::Unavailable,
            "not_exposed_by_capture_contract",
        ),
        (
            "continuous_collection",
            Status::Unverified,
            "one_snapshot_does_not_establish_gap_free_collection",
        ),
        (
            "execution_origin_authenticated",
            Status::Unverified,
            if source == Source::ClaudeCloud {
                "handoff_may_include_later_local_turns"
            } else {
                "collector_or_provider_reported_not_per_event_attestation"
            },
        ),
    ]
    .into_iter()
    .map(|(key, status, basis)| {
        (
            key.into(),
            Claim {
                status,
                basis: basis.into(),
            },
        )
    })
    .collect()
}

pub fn build(source: Source, original: &str, published: &str, compact: &str) -> Result<Contract> {
    let counts = measurements(source, original, published, compact)?;
    Ok(Contract {
        version: 1,
        source,
        source_basis: source.basis().into(),
        native_sha256: capture::hash(published),
        standardized_sha256: capture::hash(compact),
        standardized_scope:
            "compact_transcript_only; session_metadata_and_native_evidence_are_separate".into(),
        count_unit:
            "observed_values_not_unique_events; native_and_compact_counts_are_not_percentages"
                .into(),
        fields: counts
            .into_iter()
            .map(|(field, count)| {
                (
                    field,
                    FieldCoverage {
                        native: availability(count.native, count.changed, None),
                        standardized: availability(
                            count.standardized,
                            count.changed,
                            Some(count.native),
                        ),
                        native_values_changed_by_redaction: count.changed,
                    },
                )
            })
            .collect(),
        guarantees: guarantees(source),
    })
}

pub fn validate_copilot(value: &Value, full: &str, compact: &str) -> Result<()> {
    let contract: Contract = serde_json::from_value(value.clone())?;
    ensure!(
        matches!(contract.source, Source::CopilotLocal | Source::CopilotCloud),
        "Wrong coverage provider for Copilot artifact"
    );
    let mut expected = build(contract.source, full, full, compact)?;
    for (field, entry) in &contract.fields {
        let count = entry.native_values_changed_by_redaction;
        let expected_entry = expected
            .fields
            .get_mut(field)
            .expect("All fields initialized");
        ensure!(
            count <= expected_entry.native.observed_values,
            "Invalid coverage redaction count"
        );
        expected_entry.native_values_changed_by_redaction = count;
        expected_entry.native = availability(expected_entry.native.observed_values, count, None);
        expected_entry.standardized = availability(
            expected_entry.standardized.observed_values,
            count,
            Some(expected_entry.native.observed_values),
        );
    }
    ensure!(
        contract == expected,
        "Coverage contract does not match checkpoint evidence"
    );
    Ok(())
}
