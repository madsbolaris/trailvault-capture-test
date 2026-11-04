use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Provenance {
    UserPrompts,
    AssistantResponses,
    InstructionsMemory,
    SystemInstructions,
    Memory,
    SkillsTools,
    FilesRetrieved,
    ToolInteractions,
    Reasoning,
    AgentCoordination,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Activity {
    Research,
    Planning,
    Implementation,
    Validation,
    Debugging,
    Delivery,
    Unknown,
}

#[derive(Serialize)]
struct Classification {
    version: u8,
    method: &'static str,
    provenance: Provenance,
    activity: Activity,
    basis: &'static str,
    activity_basis: &'static str,
}

static INTENTS: LazyLock<Vec<(Activity, Regex)>> = LazyLock::new(|| {
    [
        (
            Activity::Research,
            r"(?i)\b(research|explore|inspect|investigate|look up)\b",
        ),
        (Activity::Planning, r"(?i)\b(plan|design|outline)\b"),
        (
            Activity::Implementation,
            r"(?i)\b(implement|add|create|modify|refactor|write)\b",
        ),
        (
            Activity::Validation,
            r"(?i)\b(run (the )?(tests?|build|lint|checks?)|validate|verify)\b",
        ),
        (
            Activity::Debugging,
            r"(?i)\b(debug|diagnose|troubleshoot)\b",
        ),
        (
            Activity::Delivery,
            r"(?i)\b(publish|commit|push|release|deploy)\b",
        ),
    ]
    .into_iter()
    .map(|(activity, pattern)| {
        (
            activity,
            Regex::new(pattern).expect("activity intent regex"),
        )
    })
    .collect()
});

fn intent(text: Option<&str>) -> Activity {
    let Some(text) = text else {
        return Activity::Unknown;
    };
    let text = text.trim();
    if text.contains(['?', '"', '`', '\n']) {
        return Activity::Unknown;
    }
    let text = if text
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("please "))
    {
        text[7..].trim_start()
    } else {
        text
    };
    if !INTENTS
        .iter()
        .any(|(_, pattern)| pattern.find(text).is_some_and(|found| found.start() == 0))
    {
        return Activity::Unknown;
    }
    let mut matches = INTENTS.iter().filter(|(_, pattern)| pattern.is_match(text));
    match (matches.next(), matches.next()) {
        (Some((activity, _)), None) => *activity,
        _ => Activity::Unknown,
    }
}

fn arguments(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| value.clone()),
        _ => value.clone(),
    }
}

fn command_activity(command: &str) -> Activity {
    // Opaque scripts and compound shells need an actual parser, not substring guesses.
    if command.contains([
        ';', '|', '&', '\n', '\r', '`', '$', '<', '>', '(', ')', '\\',
    ]) {
        return Activity::Unknown;
    }
    let parts: Vec<_> = command.split_whitespace().collect();
    let Some(program) = parts.first().and_then(|name| name.rsplit('/').next()) else {
        return Activity::Unknown;
    };
    let verb = parts.get(1).copied().unwrap_or("");
    match (program, verb) {
        ("cargo", "test" | "check" | "build" | "clippy" | "fmt")
        | ("npm" | "pnpm" | "yarn", "test" | "lint" | "build" | "typecheck")
        | ("pytest" | "vitest" | "jest" | "tsc", _)
        | ("go", "test" | "vet" | "build") => Activity::Validation,
        ("python" | "python3", "-m") if parts.get(2) == Some(&"pytest") => Activity::Validation,
        ("npm" | "pnpm" | "yarn", "run")
            if matches!(
                parts.get(2),
                Some(&("test" | "lint" | "build" | "typecheck"))
            ) =>
        {
            Activity::Validation
        }
        ("git", "status" | "log" | "show" | "diff" | "grep" | "ls-files")
        | ("rg" | "grep" | "cat" | "head" | "tail" | "ls" | "find", _) => Activity::Research,
        ("git", "commit" | "push" | "tag") => Activity::Delivery,
        ("lldb" | "gdb", _) => Activity::Debugging,
        _ => Activity::Unknown,
    }
}

fn tool(
    name: &str,
    input: &Value,
    result: bool,
    version: u8,
) -> (Provenance, Activity, &'static str) {
    let name = name.to_ascii_lowercase();
    let input = arguments(input);
    let read = matches!(
        name.as_str(),
        "read"
            | "view"
            | "read_file"
            | "grep"
            | "glob"
            | "search"
            | "web_search"
            | "web_fetch"
            | "list_directory"
    );
    let provenance = if matches!(
        name.as_str(),
        "task" | "spawn_agent" | "send_message" | "wait_agent"
    ) {
        Provenance::AgentCoordination
    } else if matches!(name.as_str(), "skill" | "activate_skill" | "list_tools") {
        Provenance::SkillsTools
    } else if version == 2 && matches!(name.as_str(), "store_memory" | "read_memory") {
        Provenance::Memory
    } else if read && result {
        let path = ["file_path", "path", "filePath"]
            .iter()
            .find_map(|key| input[*key].as_str())
            .unwrap_or("");
        let file = path.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        if matches!(
            file.as_str(),
            "agents.md" | "claude.md" | "copilot-instructions.md"
        ) || file.ends_with(".instructions.md")
        {
            if version == 1 {
                Provenance::InstructionsMemory
            } else {
                Provenance::SystemInstructions
            }
        } else if version == 2 && file == "memory.md" {
            Provenance::Memory
        } else if file == "skill.md" {
            Provenance::SkillsTools
        } else {
            Provenance::FilesRetrieved
        }
    } else {
        Provenance::ToolInteractions
    };
    let (activity, basis) = if read {
        (Activity::Research, "tool_name")
    } else if matches!(
        name.as_str(),
        "write" | "write_file" | "create" | "edit" | "str_replace" | "apply_patch"
    ) {
        (Activity::Implementation, "tool_name")
    } else if matches!(
        name.as_str(),
        "bash" | "shell" | "exec_command" | "run_command" | "exec"
    ) {
        let activity = input["command"]
            .as_str()
            .or_else(|| input["cmd"].as_str())
            .map(command_activity)
            .unwrap_or(Activity::Unknown);
        (
            activity,
            if activity == Activity::Unknown {
                "unknown"
            } else {
                "command"
            },
        )
    } else {
        (Activity::Unknown, "unknown")
    };
    (provenance, activity, basis)
}

/// Classify retained generic records, never provider billing or hidden context.
pub fn annotate(rows: &mut [Value]) -> Result<()> {
    annotate_version(rows, 2)
}

pub(crate) fn annotate_version(rows: &mut [Value], version: u8) -> Result<()> {
    ensure!(
        matches!(version, 1 | 2),
        "Unsupported classification version"
    );
    let mut calls = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|row| row["type"] == "tool.execution_start")
    {
        if let (Some(id), Some(name)) = (
            row["data"]["toolCallId"].as_str(),
            row["data"]["toolName"].as_str(),
        ) {
            let input = row["data"].get("arguments").cloned().unwrap_or(Value::Null);
            if let Some((old_name, old_input)) =
                calls.insert(id.to_owned(), (name.to_owned(), input.clone()))
            {
                ensure!(
                    old_name == name && old_input == input,
                    "Conflicting tool-call classification evidence for {id}"
                );
            }
        }
    }
    let mut user_intent = Activity::Unknown;
    for row in rows {
        let mut class = Classification {
            version,
            method: "deterministic",
            provenance: Provenance::Unknown,
            activity: Activity::Unknown,
            basis: "unknown",
            activity_basis: "unknown",
        };
        match row["type"].as_str() {
            Some("header") => continue,
            Some("message") => {
                class.basis = "role";
                match row["role"].as_str() {
                    Some("user") => {
                        class.provenance = Provenance::UserPrompts;
                        user_intent = intent(row["content"].as_str());
                        class.activity = user_intent;
                        if user_intent != Activity::Unknown {
                            class.activity_basis = "explicit_intent";
                        }
                    }
                    Some("assistant") => {
                        class.provenance = Provenance::AssistantResponses;
                        class.activity = user_intent;
                        if user_intent != Activity::Unknown {
                            class.activity_basis = "user_intent";
                        }
                    }
                    _ => {}
                }
            }
            Some("context") => {
                class.provenance = match row["kind"].as_str() {
                    Some("system.message" | "developer.message") => {
                        if version == 1 {
                            Provenance::InstructionsMemory
                        } else {
                            Provenance::SystemInstructions
                        }
                    }
                    Some("assistant.reasoning") => Provenance::Reasoning,
                    Some("skill.activated" | "tools.catalog") => Provenance::SkillsTools,
                    _ => Provenance::Unknown,
                };
                class.basis = if class.provenance == Provenance::Unknown {
                    "unknown"
                } else {
                    "event_type"
                };
            }
            Some("tool.execution_start" | "tool.execution_complete") => {
                let result = row["type"] == "tool.execution_complete";
                let call = row["data"]["toolCallId"]
                    .as_str()
                    .and_then(|id| calls.get(id));
                let direct_name = row["data"]["toolName"].as_str();
                let direct_input = &row["data"]["arguments"];
                let evidence = call
                    .map(|(name, input)| (name.as_str(), input))
                    .or_else(|| direct_name.map(|name| (name, direct_input)));
                if let Some((name, input)) = evidence {
                    let (provenance, activity, activity_basis) = tool(name, input, result, version);
                    class.provenance = if result
                        && row["data"]["success"] == false
                        && matches!(
                            provenance,
                            Provenance::FilesRetrieved
                                | Provenance::InstructionsMemory
                                | Provenance::SystemInstructions
                                | Provenance::Memory
                                | Provenance::SkillsTools
                        ) {
                        Provenance::ToolInteractions
                    } else {
                        provenance
                    };
                    class.activity = activity;
                    class.basis = if matches!(
                        class.provenance,
                        Provenance::InstructionsMemory
                            | Provenance::SystemInstructions
                            | Provenance::SkillsTools
                    ) || (class.provenance == Provenance::Memory
                        && !matches!(
                            name.to_ascii_lowercase().as_str(),
                            "store_memory" | "read_memory"
                        )) {
                        "tool_path"
                    } else {
                        "tool_name"
                    };
                    class.activity_basis = activity_basis;
                } else {
                    class.provenance = Provenance::ToolInteractions;
                }
            }
            _ => {}
        }
        row.as_object_mut()
            .context("Generic transcript record must be an object")?
            .insert("classification".into(), serde_json::to_value(class)?);
    }
    Ok(())
}
