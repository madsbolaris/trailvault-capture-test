use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub tool_use_id: String,
    pub agent_id: String,
    pub subagent_type: String,
    pub task_description: String,
    pub started_at: String,
    pub completed_at: String,
    pub transcript_unavailable_reason: String,
}

pub fn from_events(input: &Value, events: &[Value]) -> Result<Task> {
    let identity = |key: &str| -> Result<Option<&str>> {
        input
            .get(key)
            .map(|value| {
                value
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .context("Child identity must be a nonempty string")
            })
            .transpose()
    };
    let id = identity("agentId")?;
    let name = identity("agentName")?;
    ensure!(
        id.is_some() || name.is_some(),
        "Subagent stop requires agentId or agentName"
    );
    let completed: BTreeSet<_> = events
        .iter()
        .filter(|event| event["type"] == "subagent.completed")
        .filter_map(|event| {
            Some((
                event["agentId"].as_str()?,
                event["data"]["toolCallId"].as_str()?,
            ))
        })
        .collect();
    let mut matched = None;
    for event in events
        .iter()
        .filter(|event| event["type"] == "subagent.started")
    {
        let data = &event["data"];
        let Some(child) = event["agentId"].as_str().filter(|text| !text.is_empty()) else {
            continue;
        };
        let Some(tool) = data["toolCallId"].as_str().filter(|text| !text.is_empty()) else {
            continue;
        };
        if let Some(id) = id {
            if id != child {
                continue;
            }
        } else if data["agentName"].as_str() != name || completed.contains(&(child, tool)) {
            continue;
        }
        if let Some((previous_child, previous_tool, _)) = matched {
            ensure!(
                previous_child == child && previous_tool == tool,
                "Ambiguous subagent identity"
            );
        }
        matched = Some((child, tool, event));
    }
    let (child, tool, event) =
        matched.context("No unambiguous subagent evidence in parent transcript")?;
    ensure!(
        tool.len() <= 128
            && tool
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)),
        "Unsafe subagent tool-call ID"
    );
    let now = Utc::now();
    let started = match event.get("timestamp") {
        Some(value) => chrono::DateTime::parse_from_rfc3339(
            value
                .as_str()
                .context("Subagent timestamp must be a string")?,
        )?
        .with_timezone(&Utc),
        None => now,
    };
    ensure!(started <= now, "Subagent start is later than completion");
    let text = |key: &str| -> Result<&str> {
        event["data"]
            .get(key)
            .map(|value| {
                value
                    .as_str()
                    .with_context(|| format!("Subagent {key} must be a string"))
            })
            .transpose()
            .map(|value| value.unwrap_or(""))
    };
    let agent_type = text("agentType")?;
    let agent_name = text("agentName")?;
    Ok(Task {
        tool_use_id: tool.into(),
        agent_id: child.into(),
        subagent_type: if agent_type.is_empty() {
            agent_name
        } else {
            agent_type
        }
        .into(),
        task_description: text("agentDescription")?.into(),
        started_at: started.to_rfc3339(),
        completed_at: now.to_rfc3339(),
        transcript_unavailable_reason: "transcript path unresolvable".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn start(id: &str, tool: &str) -> Value {
        json!({"type":"subagent.started","agentId":id,"data":{"toolCallId":tool,"agentName":"explore","agentType":"","agentDescription":"Synthetic child"}})
    }

    #[test]
    fn name_only_matches_one_incomplete_child_and_empty_type_falls_back_to_name() {
        let task = from_events(&json!({"agentName":"explore"}), &[start("one", "task-1")]).unwrap();
        assert_eq!(task.agent_id, "one");
        assert_eq!(task.subagent_type, "explore");
        assert_eq!(task.started_at, task.completed_at);
    }

    #[test]
    fn completed_children_are_excluded_only_for_name_based_resolution() {
        let events = vec![
            start("one", "task-1"),
            start("two", "task-2"),
            json!({"type":"subagent.completed","agentId":"one","data":{"toolCallId":"task-1"}}),
        ];
        assert_eq!(
            from_events(&json!({"agentName":"explore"}), &events)
                .unwrap()
                .agent_id,
            "two"
        );
        assert_eq!(
            from_events(&json!({"agentId":"one"}), &events)
                .unwrap()
                .agent_id,
            "one"
        );
    }

    #[test]
    fn repeated_identity_is_allowed_but_conflicting_mappings_are_not() {
        assert!(
            from_events(
                &json!({"agentId":"one"}),
                &[start("one", "task-1"), start("one", "task-1")]
            )
            .is_ok()
        );
        assert!(
            from_events(
                &json!({"agentId":"one"}),
                &[start("one", "task-1"), start("one", "task-2")]
            )
            .is_err()
        );
        assert!(
            from_events(
                &json!({"agentName":"explore"}),
                &[start("one", "task-1"), start("two", "task-2")]
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_identity_paths_metadata_and_times_fail_explicitly() {
        for input in [
            json!({}),
            json!({"agentId":null}),
            json!({"agentId":""}),
            json!({"agentName":2}),
        ] {
            assert!(from_events(&input, &[start("one", "task-1")]).is_err());
        }
        for tool in ["../escape", "a/b", "a\\b", ".", ".."] {
            assert!(from_events(&json!({"agentId":"one"}), &[start("one", tool)]).is_err());
        }
        for (field, value) in [
            ("agentDescription", json!(42)),
            ("agentType", Value::Null),
            ("agentName", json!([])),
        ] {
            let mut event = start("one", "task-1");
            event["data"][field] = value;
            assert!(from_events(&json!({"agentId":"one"}), &[event]).is_err());
        }
        for time in [
            json!("not-a-time"),
            json!("9999-01-01T00:00:00Z"),
            json!(42),
        ] {
            let mut event = start("one", "task-1");
            event["timestamp"] = time;
            assert!(from_events(&json!({"agentId":"one"}), &[event]).is_err());
        }
    }
}
