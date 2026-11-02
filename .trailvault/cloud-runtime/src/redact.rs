use anyhow::{Context, Result, ensure};
use regex::{Captures, Regex};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::LazyLock};

static PROVIDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,255}|github_pat_[A-Za-z0-9_]{40,255}|AKIA[A-Z0-9]{16}|sk-(?:proj-)?[A-Za-z0-9_-]{32,255})\b").expect("provider regex")
});
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b([A-Za-z0-9_]*(?:API_KEY|PASSWORD|SECRET|ACCESS_TOKEN)[A-Za-z0-9_]*\s*[=:]\s*["']?)([^\s"',;]{4,})"#).expect("assignment regex")
});
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\b[a-z][a-z0-9+.-]*://[^\s/:@]+:)[^\s/@]+@").expect("URL regex")
});
static QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([?;&](?:password|passwd|pwd)=)[^&;\s]+").expect("query regex")
});
static ENTROPY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9_+/=-]{32,256}\b").expect("entropy regex"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[A-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Z0-9-]+(?:\.[A-Z0-9-]+)+\b")
        .expect("email regex")
});
static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\+?1[- .]?)?\(?[2-9][0-9]{2}\)?[- .][0-9]{3}[- .][0-9]{4}")
        .expect("phone regex")
});
static IP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b").expect("IP regex"));
static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b[0-9]{1,6}\s+(?:[A-Za-z]+\s+){1,4}(?:Street|Road|Avenue|Lane|Drive|St|Rd|Ave)\b",
    )
    .expect("address regex")
});

pub struct Redactor {
    custom: Vec<Regex>,
    pii: bool,
    address: bool,
    pub replacements: usize,
}

impl Redactor {
    pub fn new(settings: &Value) -> Result<Self> {
        let empty = json!({});
        let config = settings.get("redaction").unwrap_or(&empty);
        ensure!(config.is_object(), "redaction must be an object");
        let mut custom = Vec::new();
        if let Some(patterns) = config.get("custom_redactions") {
            let patterns = patterns
                .as_object()
                .context("custom_redactions must be a label-to-regex object")?;
            for (label, pattern) in patterns {
                let pattern = pattern
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .with_context(|| format!("Invalid custom redaction {label}"))?;
                let compiled = Regex::new(pattern)
                    .with_context(|| format!("Invalid custom redaction {label}"))?;
                ensure!(
                    !compiled.is_match(""),
                    "Custom redaction {label} matches empty input"
                );
                custom.push(compiled);
            }
        }
        let pii = config.get("pii").unwrap_or(&empty);
        ensure!(pii.is_object(), "redaction.pii must be an object");
        let enabled = match pii.get("enabled") {
            Some(value) => value
                .as_bool()
                .context("redaction.pii.enabled must be boolean")?,
            None => false,
        };
        let address = match pii.pointer("/street_address/enabled") {
            Some(value) => value
                .as_bool()
                .context("redaction.pii.street_address.enabled must be boolean")?,
            None => false,
        };
        Ok(Self {
            custom,
            pii: enabled,
            address,
            replacements: 0,
        })
    }

    pub fn text(&mut self, input: &str) -> String {
        let count = &mut self.replacements;
        let mut value = replace(&PROVIDER, input, count, |_| "REDACTED".into());
        value = replace(&ASSIGNMENT, &value, count, |c| {
            let secret = c[2].to_ascii_lowercase();
            if ["your-", "example", "placeholder", "redacted"]
                .iter()
                .any(|prefix| secret.starts_with(prefix))
            {
                c[0].to_owned()
            } else {
                format!("{}REDACTED", &c[1])
            }
        });
        value = replace(&URL, &value, count, |c| format!("{}REDACTED@", &c[1]));
        value = replace(&QUERY, &value, count, |c| format!("{}REDACTED", &c[1]));
        value = ENTROPY
            .replace_all(&value, |c: &Captures<'_>| {
                let token = &c[0];
                if !token.bytes().any(|c| c.is_ascii_lowercase())
                    || !token.bytes().any(|c| c.is_ascii_uppercase())
                    || !token.bytes().any(|c| c.is_ascii_digit())
                {
                    return token.to_owned();
                }
                let mut frequencies = BTreeMap::new();
                for byte in token.bytes() {
                    *frequencies.entry(byte).or_insert(0_u32) += 1;
                }
                let entropy: f64 = frequencies
                    .values()
                    .map(|n| {
                        let p = f64::from(*n) / token.len() as f64;
                        -p * p.log2()
                    })
                    .sum();
                if entropy < 4.5 {
                    token.to_owned()
                } else {
                    *count += 1;
                    "REDACTED".into()
                }
            })
            .into_owned();
        if self.pii {
            value = replace(&EMAIL, &value, count, |_| "[REDACTED_EMAIL]".into());
            let original = value.clone();
            value = replace(&PHONE, &value, count, |c| {
                let m = c.get(0).expect("matched phone");
                let word = |c: char| c.is_alphanumeric() || c == '_';
                if original[..m.start()].chars().next_back().is_some_and(word)
                    || original[m.end()..].chars().next().is_some_and(word)
                {
                    c[0].into()
                } else {
                    "[REDACTED_PHONE]".into()
                }
            });
            value = replace(&IP, &value, count, |c| {
                if c[0]
                    .split('.')
                    .any(|p| !p.parse::<u16>().is_ok_and(|p| p <= 255))
                {
                    c[0].into()
                } else {
                    "[REDACTED_IP]".into()
                }
            });
            if self.address {
                value = replace(&ADDRESS, &value, count, |_| "[REDACTED_ADDRESS]".into());
            }
        }
        for pattern in &self.custom {
            value = replace(pattern, &value, count, |_| "REDACTED".into());
        }
        value
    }

    pub fn event(&mut self, value: &Value) -> Value {
        self.walk(value, false)
    }
    fn walk(&mut self, value: &Value, active: bool) -> Value {
        match value {
            Value::String(text) if active => Value::String(self.text(text)),
            Value::Array(values) => {
                Value::Array(values.iter().map(|v| self.walk(v, active)).collect())
            }
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, item)| {
                        let structural = matches!(
                            key.as_str(),
                            "cwd"
                                | "gitRoot"
                                | "path"
                                | "filePath"
                                | "transcriptPath"
                                | "sessionId"
                                | "id"
                                | "parentId"
                                | "type"
                                | "timestamp"
                                | "toolCallId"
                                | "name"
                        );
                        let text = matches!(
                            key.as_str(),
                            "content"
                                | "transformedContent"
                                | "prompt"
                                | "initialPrompt"
                                | "output"
                                | "text"
                                | "reasoningText"
                                | "result"
                                | "toolResult"
                                | "textResultForLlm"
                                | "sessionLog"
                                | "arguments"
                                | "toolArgs"
                                | "error"
                                | "message"
                        );
                        (
                            key.clone(),
                            self.walk(item, !structural && (active || text)),
                        )
                    })
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
}

fn replace(
    regex: &Regex,
    input: &str,
    count: &mut usize,
    mut replacement: impl FnMut(&Captures<'_>) -> String,
) -> String {
    regex
        .replace_all(input, |captures: &Captures<'_>| {
            *count += 1;
            replacement(captures)
        })
        .into_owned()
}

pub fn parse(content: &str) -> Result<Vec<Value>> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let event: Value = serde_json::from_str(line)
                .with_context(|| format!("Invalid transcript line {}", index + 1))?;
            ensure!(
                event["type"].is_string(),
                "Invalid transcript line {}: event must have a string type",
                index + 1
            );
            Ok(event)
        })
        .collect()
}
pub fn jsonl(events: &[Value]) -> Result<String> {
    Ok(events
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n")
        + "\n")
}
pub fn compact(events: &[Value]) -> Result<String> {
    let mut rows = vec![json!({"type":"header","version":1,"agent":"Copilot CLI"})];
    let mut children = std::collections::BTreeMap::new();
    for event in events
        .iter()
        .filter(|event| event["type"] == "subagent.started")
    {
        if let (Some(tool), Some(child)) = (
            event["data"]["toolCallId"].as_str(),
            event["agentId"].as_str(),
        ) {
            if !tool.is_empty() && !child.is_empty() {
                if let Some(previous) = children.insert(tool, child) {
                    anyhow::ensure!(previous == child, "Ambiguous subagent tool-call identity");
                }
            }
        }
    }
    for event in events {
        match event["type"].as_str() {
            Some(kind @ ("user.message" | "assistant.message")) => rows.push(json!({
                "type":"message", "role":kind.split('.').next(), "content":event["data"].get("content").cloned().unwrap_or(json!(""))
            })),
            Some(kind @ ("tool.execution_start" | "tool.execution_complete")) => {
                let mut data=event["data"].clone();
                if kind=="tool.execution_complete" {
                    if let Some(child)=data["toolCallId"].as_str().and_then(|id|children.get(id)) {
                        if data.get("result").is_none_or(Value::is_null) {
                            data["result"]=json!({});
                        }
                        anyhow::ensure!(data["result"].is_object(),"Subagent tool result must be an object");
                        data["result"]["agent"]=json!({"id":child});
                    }
                }
                rows.push(json!({"type":kind,"data":data}));
            }
            _ => {}
        }
    }
    jsonl(&rows)
}
