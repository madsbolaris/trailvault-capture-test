use crate::{capture, git::Repo, redact};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[serde(rename = "claude-code")]
    Claude,
    #[serde(rename = "codex")]
    Codex,
}

impl Provider {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "claude-code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => anyhow::bail!("Unsupported native provider {name}"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "Claude Code" => Some(Self::Claude),
            "Codex" => Some(Self::Codex),
            _ => None,
        }
    }
    pub fn transcript(self, path: &Path, id: &str) -> Result<PathBuf> {
        ensure!(
            path.is_absolute(),
            "Native transcript path must be absolute"
        );
        let home = env::var_os(match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
        })
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(match self {
                Self::Claude => ".claude",
                Self::Codex => ".codex",
            })
        });
        ensure!(home.is_absolute(), "Native provider home must be absolute");
        let root = home
            .join(match self {
                Self::Claude => "projects",
                Self::Codex => "sessions",
            })
            .canonicalize()
            .context("Cannot resolve native provider transcript root")?;
        let resolved = path
            .canonicalize()
            .context("Cannot open native transcript")?;
        ensure!(
            resolved.starts_with(&root),
            "Native transcript must remain inside {}",
            root.display()
        );
        let filename = resolved
            .file_name()
            .and_then(|n| n.to_str())
            .context("Non-UTF8 native filename")?;
        ensure!(
            match self {
                Self::Claude => filename == format!("{id}.jsonl"),
                Self::Codex => filename.ends_with(&format!("-{id}.jsonl")),
            },
            "Native transcript filename does not match the session"
        );
        ensure!(
            fs::metadata(&resolved)?.len() <= 64 * 1024 * 1024,
            "Native transcript exceeds 64 MiB"
        );
        Ok(resolved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub provider: Provider,
    pub boundary: String,
    pub observed_at: String,
    pub source_head_sha: String,
    pub branch: String,
    pub dirty_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted: Option<crate::hosted::Provenance>,
}

pub struct Parsed {
    pub canonical: Vec<Value>,
    pub observations: Vec<Value>,
    pub references: Vec<Value>,
    pub pending_calls: BTreeSet<String>,
}

fn references(text: &str, role: &str, line: usize, output: &mut Vec<Value>) -> Result<()> {
    for target in crate::relationships::mentions(text, None)? {
        output.push(
            json!({"host":target.host,"repository":target.repository,"number":target.number,
            "url":target.url(),
            "source_role":role,"native_line":line,
            "relationship":if role=="provider_link" {"provider_linked"} else {"mentioned"},
            "retrieval_verified":false,"contribution_verified":false}),
        );
    }
    Ok(())
}

pub(crate) fn message(parsed: &mut Parsed, role: &str, text: &str, line: usize) -> Result<()> {
    parsed
        .canonical
        .push(json!({"type":format!("{role}.message"),"data":{"content":text}}));
    references(text, role, line, &mut parsed.references)?;
    Ok(())
}

pub(crate) fn tool_start(
    parsed: &mut Parsed,
    id: &Value,
    name: &Value,
    arguments: &Value,
    line: usize,
) -> Result<()> {
    let id = id
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Native tool start has no call ID")?;
    let name = name
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Native tool start has no name")?;
    parsed.pending_calls.insert(id.into());
    let data = json!({"toolCallId":id,"toolName":name,"arguments":arguments});
    parsed
        .canonical
        .push(json!({"type":"tool.execution_start","data":data}));
    parsed.observations.push(json!({"kind":"tool.started","native_line":line,"call_id":id,"tool":name,"arguments":arguments}));
    references(
        &crate::relationships::reference_text(arguments),
        "tool_input",
        line,
        &mut parsed.references,
    )?;
    Ok(())
}

pub(crate) fn tool_result(
    parsed: &mut Parsed,
    id: &Value,
    output: &Value,
    failed: bool,
    line: usize,
) -> Result<()> {
    let id = id
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Native tool result has no call ID")?;
    parsed.pending_calls.remove(id);
    let mut children = vec![];
    if let Some(text) = output.as_str() {
        if let Ok(Value::Array(blocks)) = serde_json::from_str::<Value>(text) {
            for block in blocks {
                if let Some(text) = block["text"].as_str() {
                    if let Ok(value) = serde_json::from_str::<Value>(text) {
                        if value.get("exit_code").is_some() || value.get("error").is_some() {
                            children.push(value);
                        }
                    }
                }
            }
        }
    }
    let failed = failed
        || children
            .iter()
            .any(|c| c["exit_code"].as_i64().is_some_and(|n| n != 0) || c.get("error").is_some());
    let data =
        json!({"toolCallId":id,"success":!failed,"result":{"output":output,"children":children}});
    parsed
        .canonical
        .push(json!({"type":"tool.execution_complete","data":data}));
    parsed.observations.push(
        json!({"kind":"tool.finished","native_line":line,"call_id":id,
        "status":if failed {"failed"} else {"returned"},"output":output,"child_results":children,
        "note":"A returned wrapper is not proof every nested action succeeded"}),
    );
    references(
        &crate::relationships::reference_text(output),
        "tool_output",
        line,
        &mut parsed.references,
    )?;
    Ok(())
}

pub fn parse(provider: Provider, id: &str, cwd: &Path, raw: &str) -> Result<Parsed> {
    parse_inner(provider, id, cwd, raw, false)
}

pub fn parse_hosted(id: &str, cwd: &Path, raw: &str) -> Result<Parsed> {
    parse_inner(Provider::Claude, id, cwd, raw, true)
}

fn parse_inner(
    provider: Provider,
    id: &str,
    cwd: &Path,
    raw: &str,
    hosted: bool,
) -> Result<Parsed> {
    let events = redact::parse(raw)?;
    ensure!(!events.is_empty(), "Native transcript is empty");
    let mut identity = false;
    let mut directory = false;
    let cwd = if hosted {
        cwd.to_owned()
    } else {
        cwd.canonicalize()?
    };
    let mut parsed = Parsed {
        canonical: vec![],
        observations: vec![],
        references: vec![],
        pending_calls: BTreeSet::new(),
    };
    let lines = raw
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index + 1);
    for (event, line) in events.iter().zip(lines) {
        if provider == Provider::Claude {
            if let Some(session) = event.get("sessionId") {
                ensure!(
                    session == id,
                    "Claude transcript contains a different session identity"
                );
                identity = true;
            }
            if let Some(path) = event["cwd"].as_str() {
                ensure!(
                    if hosted {
                        Path::new(path) == cwd
                    } else {
                        Path::new(path).canonicalize()?.starts_with(&cwd)
                    },
                    "Claude transcript belongs to a different worktree"
                );
                directory = true;
            }
            match event["type"].as_str() {
                Some("user" | "assistant") => {
                    let role = event["type"].as_str().context("Missing message role")?;
                    let content = &event["message"]["content"];
                    if let Some(text) = content.as_str() {
                        message(&mut parsed, role, text, line)?;
                    }
                    if let Some(blocks) = content.as_array() {
                        for block in blocks {
                            match block["type"].as_str() {
                                Some("text") => message(
                                    &mut parsed,
                                    role,
                                    block["text"]
                                        .as_str()
                                        .context("Native text must be a string")?,
                                    line,
                                )?,
                                Some("tool_use") => tool_start(
                                    &mut parsed,
                                    &block["id"],
                                    &block["name"],
                                    &block["input"],
                                    line,
                                )?,
                                Some("tool_result") => tool_result(
                                    &mut parsed,
                                    &block["tool_use_id"],
                                    &block["content"],
                                    block["is_error"] == true,
                                    line,
                                )?,
                                Some("thinking") => {
                                    if let Some(text) = block["thinking"].as_str() {
                                        parsed.canonical.push(json!({"type":"assistant.reasoning","data":{"content":text}}));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some("system") => {
                    if let Some(text) = event["content"].as_str() {
                        parsed
                            .canonical
                            .push(json!({"type":"system.message","data":{"content":text}}));
                    }
                }
                Some("pr-link") => references(
                    event["prUrl"]
                        .as_str()
                        .context("Native PR link lacks URL")?,
                    "provider_link",
                    line,
                    &mut parsed.references,
                )?,
                _ => {}
            }
        } else {
            if event["type"] == "session_meta" {
                ensure!(
                    event["payload"]["id"] == id,
                    "Codex transcript contains a different session identity"
                );
                let path = event["payload"]["cwd"]
                    .as_str()
                    .context("Codex session metadata lacks cwd")?;
                ensure!(
                    Path::new(path).canonicalize()?.starts_with(&cwd),
                    "Codex transcript belongs to a different worktree"
                );
                identity = true;
                directory = true;
            }
            if event["type"] == "response_item" {
                let payload = &event["payload"];
                match payload["type"].as_str() {
                    Some("message") => {
                        if let Some(role @ ("user" | "assistant" | "developer" | "system")) =
                            payload["role"].as_str()
                        {
                            if let Some(blocks) = payload["content"].as_array() {
                                for block in blocks {
                                    if let Some(text) = block["text"].as_str() {
                                        message(&mut parsed, role, text, line)?;
                                    }
                                }
                            }
                        }
                    }
                    Some("reasoning") => {
                        if let Some(blocks) = payload["summary"].as_array() {
                            for block in blocks {
                                if let Some(text) = block["text"].as_str() {
                                    parsed
                                        .canonical
                                        .push(json!({"type":"assistant.reasoning","data":{
                                        "content":text,"representation":"summary"}}));
                                }
                            }
                        }
                    }
                    Some("custom_tool_call" | "function_call") => {
                        let arguments = payload
                            .get("input")
                            .or_else(|| payload.get("arguments"))
                            .context("Native call lacks input/arguments")?;
                        tool_start(
                            &mut parsed,
                            &payload["call_id"],
                            &payload["name"],
                            arguments,
                            line,
                        )?;
                    }
                    Some("custom_tool_call_output" | "function_call_output") => tool_result(
                        &mut parsed,
                        &payload["call_id"],
                        &payload["output"],
                        payload["is_error"] == true,
                        line,
                    )?,
                    _ => {}
                }
            }
        }
        parsed
            .observations
            .push(json!({"kind":"native.event","native_line":line,
            "native_type":event["type"],"timestamp":event.get("timestamp"),"event":event}));
    }
    ensure!(
        identity,
        "Native transcript has no matching session identity"
    );
    ensure!(
        directory,
        "Native transcript has no verified worktree identity"
    );
    Ok(parsed)
}

pub fn checkpoint(
    repo: &Repo,
    session: &str,
    raw: &str,
    context: &ContextSnapshot,
    redactor: &mut redact::Redactor,
) -> Result<(String, String, Value)> {
    let api = context
        .hosted
        .as_ref()
        .is_some_and(|h| h.provider() == Provider::Codex);
    let (full, parsed, api_summary) = if api {
        let hosted = context.hosted.as_ref().context("Missing hosted context")?;
        let (full, parsed, summary) = crate::codex_api::checkpoint(
            session,
            Path::new(&hosted.transcript_cwd),
            raw,
            redactor,
        )?;
        (full, parsed, Some(summary))
    } else {
        let events: Vec<_> = redact::parse(raw)?
            .iter()
            .map(|e| redactor.native_event(e))
            .collect();
        let full = redact::jsonl(&events)?;
        let parsed = if let Some(hosted) = &context.hosted {
            hosted.parse(session, &full)?
        } else {
            parse(context.provider, session, &repo.root, &full)?
        };
        (full, parsed, None)
    };
    let mut compact = redact::parse(&redact::compact(&parsed.canonical)?)?;
    compact[0]["agent"] = json!(context.provider.label());
    if let Some(hosted) = &context.hosted {
        compact[0]["agent"] = json!(hosted.label());
    }
    let activity = if context.hosted.is_some() {
        "unknown"
    } else if context.boundary == "session-end" {
        "ended"
    } else if context.boundary == "agent-stop" {
        "idle"
    } else if !parsed.pending_calls.is_empty() {
        "working"
    } else {
        "unknown"
    };
    let mut evidence = json!({"version":1,"provider":context.provider.name(),"session_id":session,
        "observed_at":context.observed_at,"source_head_sha":context.source_head_sha,"branch":context.branch,
        "dirty_files":context.dirty_files,"native_sha256":capture::hash(&full),
        "lifecycle":{"boundary":context.boundary,"state":activity,"pending_tool_calls":parsed.pending_calls,
            "presence":"historical_snapshot","live_lease":false,"task_completion":"unverified",
            "background_jobs":"unverified","basis":"native transcript plus explicit collector boundary; not a current presence lease"},
        "observations":parsed.observations,"pr_references":parsed.references,
        "publication":{"kind":"session_snapshot","contribution_verified":false}});
    if let Some(hosted) = &context.hosted {
        evidence["hosted"] = serde_json::to_value(hosted)?;
        evidence["hosted"]["source_branch"] = json!(redactor.text(&hosted.source_branch));
        evidence["hosted"]["transcript_scope"] = json!(if api {
            "available persisted app-server history; not original streaming events"
        } else {
            "handoff_copy; per-record cloud versus local execution not independently authenticated"
        });
        if let Some(observations) = evidence["observations"].as_array_mut() {
            for observation in observations {
                observation["execution_location"] = json!(if api {
                    "provider_recorded; tool environment not independently authenticated"
                } else {
                    "unverified_handoff_copy"
                });
            }
        }
        evidence["lifecycle"]["basis"] = json!(if api {
            "persisted API history; provider-reported thread/turn status is not a live lease or proof of remote termination"
        } else {
            "explicitly imported hosted transcript; handoff/client exit is not remote termination"
        });
        evidence["lifecycle"]["pending_calls_are_historical"] = json!(true);
    }
    if let Some(summary) = api_summary {
        evidence["api_history"] = summary;
    }
    Ok((full, redact::jsonl(&compact)?, evidence))
}
