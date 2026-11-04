use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize)]
pub struct Case {
    pub name: String,
    pub settings: Value,
    pub sessions: Vec<Session>,
    pub partial_commit: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Session {
    pub turns: Vec<Turn>,
    pub subagent_stops: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Turn {
    pub prompt: String,
    pub events: Vec<Value>,
}

fn event(kind: &str, data: Value) -> Value {
    json!({"type":kind,"data":data})
}

fn assistant(data: Value) -> Value {
    event("assistant.message", data)
}

fn request(id: &str, name: &str, arguments: Value) -> Value {
    json!({"toolCallId":id,"name":name,"arguments":arguments})
}

fn complete(id: &str, success: bool, result: Value) -> Value {
    event(
        "tool.execution_complete",
        json!({"toolCallId":id,"success":success,"result":result}),
    )
}

fn turn(prompt: &str, events: Vec<Value>) -> Turn {
    let mut all = vec![event("user.message", json!({"content":prompt}))];
    all.extend(events);
    Turn {
        prompt: prompt.into(),
        events: all,
    }
}

fn case(name: &str, events: Vec<Value>) -> Case {
    Case {
        name: name.into(),
        settings: json!({"enabled":true,"commit_linking":"always","strategy_options":{"push_sessions":false}}),
        sessions: vec![Session {
            turns: vec![turn("Update the synthetic fixture.", events)],
            subagent_stops: vec![],
        }],
        partial_commit: false,
    }
}

fn tool(name: &str, arguments: Value, result: Value, success: bool) -> Vec<Value> {
    vec![
        assistant(
            json!({"messageId":"message-tool","content":"Using a tool.","toolRequests":[request("call-1",name,arguments.clone())]}),
        ),
        event(
            "tool.execution_start",
            json!({"toolCallId":"call-1","toolName":name,"arguments":arguments}),
        ),
        complete("call-1", success, result),
    ]
}

pub fn cases() -> Vec<Case> {
    let mut cases = vec![
        case("text-basic", vec![assistant(json!({"content":"Done."}))]),
        case(
            "text-unicode",
            vec![assistant(
                json!({"content":"Gr\u{00fc}\u{00df}e \u{65e5}\u{672c}\u{8a9e} \u{1f6b2} e\u{301}"}),
            )],
        ),
        case(
            "text-markdown-multiline",
            vec![assistant(
                json!({"content":"# Result\n\n```rust\nfn main() {}\n```\n\tTabbed.\r\n"}),
            )],
        ),
        case(
            "user-empty",
            vec![assistant(json!({"content":"No prompt."}))],
        ),
        case("assistant-empty", vec![assistant(json!({"content":""}))]),
        case("assistant-missing-content", vec![assistant(json!({}))]),
        case(
            "assistant-identity-timestamp-tokens",
            vec![assistant(
                json!({"messageId":"message-42","content":"Identified.","outputTokens":42}),
            )],
        ),
        case(
            "assistant-zero-tokens",
            vec![assistant(
                json!({"messageId":"","content":"Zero.","outputTokens":0}),
            )],
        ),
        case(
            "assistant-missing-timestamp",
            vec![assistant(
                json!({"messageId":"without-time","content":"No timestamp."}),
            )],
        ),
        case(
            "assistant-multiple-before-user",
            vec![
                assistant(json!({"messageId":"a","content":"First."})),
                assistant(json!({"messageId":"b","content":"Second."})),
            ],
        ),
    ];
    cases[3].sessions[0].turns[0] = turn("", vec![assistant(json!({"content":"No prompt."}))]);
    cases[8].sessions[0].turns[0].events[1]["timestamp"] = Value::Null;

    for (name, args, result, success) in [
        (
            "bash",
            json!({"command":"printf fixture"}),
            json!({"content":"fixture","detailedContent":"longer output"}),
            true,
        ),
        (
            "bash-error",
            json!({"command":"exit 3"}),
            json!({"content":"exit status 3"}),
            false,
        ),
        (
            "view",
            json!({"path":"/fixture/project/src/main.rs","view_range":[1,3]}),
            json!({"content":"fn main() {}\n"}),
            true,
        ),
        (
            "create",
            json!({"path":"new.rs","file_text":"fn main() {}\n"}),
            json!({"content":"Created new.rs"}),
            true,
        ),
        (
            "edit",
            json!({"path":"main.rs","old_str":"old","new_str":"new"}),
            json!({"content":"Edited main.rs"}),
            true,
        ),
        (
            "str_replace",
            json!({"path":"main.rs","old_str":"x","new_str":"y"}),
            json!({"content":"Replaced"}),
            true,
        ),
        (
            "grep",
            json!({"pattern":"TODO","path":"src"}),
            json!({"content":"src/a.rs:1:TODO","matchCount":1}),
            true,
        ),
        (
            "glob",
            json!({"pattern":"**/*.rs"}),
            json!({"content":"src/a.rs\nsrc/b.rs"}),
            true,
        ),
        (
            "list_directory",
            json!({"path":"/fixture/project"}),
            json!({"content":"src/\nREADME.md"}),
            true,
        ),
        (
            "web_fetch",
            json!({"url":"https://example.invalid/synthetic"}),
            json!({"content":"Synthetic page"}),
            true,
        ),
        (
            "web_search",
            json!({"query":"synthetic documentation"}),
            json!({"content":"Synthetic results"}),
            true,
        ),
        (
            "mcp__fixture__lookup",
            json!({"query":{"ids":[1,2],"exact":true}}),
            json!({"content":"{\"answer\":42}","structuredContent":{"answer":42}}),
            true,
        ),
        (
            "future.unknown_tool",
            json!({"future":{"nested":[true,null,3.5]}}),
            json!({"content":"Unknown but valid tool name"}),
            true,
        ),
    ] {
        let tool_name = if name == "bash-error" { "bash" } else { name };
        cases.push(case(
            &format!("tool-{name}"),
            tool(tool_name, args, result, success),
        ));
    }
    for (name, arguments) in [
        ("string", json!("literal argument")),
        ("array", json!([1,"two",false,null,{"nested":true}])),
        ("null", Value::Null),
        ("boolean", json!(false)),
        ("number", json!(42.25)),
    ] {
        cases.push(case(
            &format!("arguments-{name}"),
            tool("fixture", arguments, json!({"content":"ok"}), true),
        ));
    }
    cases.push(case(
        "arguments-absent-tool-only",
        vec![
            assistant(json!({"toolRequests":[{"toolCallId":"call-1","name":"fixture"}]})),
            complete("call-1", true, json!({"content":"ok"})),
        ],
    ));
    cases.push(case(
        "result-detailed-fallback",
        tool(
            "fixture",
            json!({}),
            json!({"content":"","detailedContent":"fallback output"}),
            true,
        ),
    ));
    cases.push(case(
        "result-empty",
        tool(
            "fixture",
            json!({}),
            json!({"content":"","detailedContent":""}),
            true,
        ),
    ));
    cases.push(case(
        "result-missing-completion",
        vec![assistant(
            json!({"toolRequests":[request("pending","fixture",json!({}))]}),
        )],
    ));
    cases.push(case(
        "result-orphan-and-unmatched",
        vec![
            complete("orphan", false, json!({"content":"Before assistant"})),
            assistant(
                json!({"content":"Pending","toolRequests":[request("real","fixture",json!({}))]}),
            ),
            complete("wrong", true, json!({"content":"Not this tool"})),
            complete("", true, json!({"content":"No identity"})),
        ],
    ));
    cases.push(case("result-duplicate-request-id", vec![
        assistant(json!({"toolRequests":[request("same","first",json!(1)),request("same","second",json!(2))]})),
        complete("same",true,json!({"content":"Last matching request"})),
    ]));
    cases.push(case(
        "result-duplicate-completion",
        vec![
            assistant(json!({"toolRequests":[request("call-1","fixture",json!({}))]})),
            complete("call-1", true, json!({"content":"First result"})),
            complete("call-1", false, json!({"content":"Replacement result"})),
        ],
    ));
    cases.push(case("parallel-out-of-order", vec![
        assistant(json!({"toolRequests":[request("a","view",json!({"path":"a"})),request("b","grep",json!({"pattern":"b"}))]})),
        complete("b",false,json!({"content":"B first"})),
        complete("a",true,json!({"content":"A second"})),
    ]));
    for (name, count, stops) in [
        ("subagent-link", 1, false),
        ("subagent-parallel", 2, false),
        ("subagent-durable-task", 1, true),
    ] {
        let requests: Vec<_> = (0..count)
            .map(|i| {
                request(
                    &format!("task-{i}"),
                    "task",
                    json!({"agent_type":"explore","prompt":"Inspect synthetic file"}),
                )
            })
            .collect();
        let mut events = vec![assistant(json!({"toolRequests":requests}))];
        let mut ids = vec![];
        for i in 0..count {
            let id = format!("10000000-0000-4000-8000-{i:012}");
            ids.push(id.clone());
            events.push(json!({"type":"subagent.started","agentId":id,"data":{"toolCallId":format!("task-{i}"),"agentName":"explore","agentType":"explore","agentDescription":"Inspect synthetic file"}}));
            events.push(json!({"type":"tool.execution_complete","agentId":id,"data":{"toolCallId":format!("child-{i}"),"parentToolCallId":format!("task-{i}"),"toolName":"edit","success":true,"arguments":{"path":format!("agent-{i}.txt")},"result":{"content":"Child edit"}}}));
            events.push(complete(
                &format!("task-{i}"),
                true,
                json!({"content":"Child done"}),
            ));
        }
        let mut fixture = case(name, events);
        if stops {
            fixture.sessions[0].subagent_stops = ids;
        }
        cases.push(fixture);
    }
    cases.push(case("lifecycle-model-usage", vec![
        event("session.model_change",json!({"previousModel":"fixture-old","newModel":"fixture-model"})),
        assistant(json!({"content":"Usage.","messageId":"usage","outputTokens":7,"model":"fixture-model"})),
        event("session.shutdown",json!({"modelMetrics":{"fixture-model":{"requests":{"count":2},"usage":{"inputTokens":100,"outputTokens":7,"cacheReadTokens":11,"cacheWriteTokens":13}}}})),
    ]));
    cases.push(case(
        "lifecycle-permission-error-cancel",
        vec![
            event(
                "permission.requested",
                json!({"toolCallId":"blocked","permission":"shell"}),
            ),
            event(
                "permission.denied",
                json!({"toolCallId":"blocked","reason":"Synthetic denial"}),
            ),
            event(
                "session.error",
                json!({"errorType":"fixture","message":"Synthetic failure"}),
            ),
            event("abort", json!({"reason":"User cancelled"})),
            assistant(json!({"content":"Cancelled."})),
        ],
    ));
    cases.push(case(
        "lifecycle-compaction-future-thinking",
        vec![
            event("session.compaction_start", json!({"reason":"context"})),
            event(
                "session.compaction_complete",
                json!({"summary":"Synthetic retained summary"}),
            ),
            event(
                "assistant.reasoning",
                json!({"content":"Synthetic reasoning fixture, not real model reasoning."}),
            ),
            event(
                "future.event",
                json!({"unknown":{"keep":[null,true,42]},"content":"Unrecognized native event"}),
            ),
            assistant(json!({"content":"After compaction.","futureField":{"keep":true}})),
        ],
    ));
    cases.push(case("attachments-image-and-binary-reference", vec![
        event("user.message",json!({"content":"Review attachments.","attachments":[{"type":"image","path":"/fixture/private/screenshot.png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB","mimeType":"image/png"},{"type":"file","path":"/fixture/private/archive.bin","bytes":[0,255,1]}]})),
        assistant(json!({"content":"Attachments reviewed."})),
    ]));
    cases.push(case("privacy-default-nested-copies", vec![
        event("user.message",json!({"content":"password=synthetic-secret-271828","transformedContent":"password=synthetic-secret-271828"})),
        assistant(json!({"content":"password=synthetic-secret-271828","toolRequests":[request("s","bash",json!({"command":"echo password=synthetic-secret-271828"}))]})),
        event("tool.execution_complete",json!({"toolCallId":"s","success":true,"result":{"content":"password=synthetic-secret-271828"},"toolResult":{"textResultForLlm":"password=synthetic-secret-271828","sessionLog":"password=synthetic-secret-271828"}})),
    ]));
    let mut custom = case(
        "privacy-custom-paths-profanity",
        vec![
            event(
                "user.message",
                json!({"content":"PRIVATE-LAB-42 /Users/fixture/secret-project damn","cwd":"/Users/fixture/secret-project"}),
            ),
            assistant(json!({"content":"PRIVATE-LAB-42 /Users/fixture/secret-project damn"})),
        ],
    );
    custom.settings["redaction"] = json!({"custom_redactions":{"project":"PRIVATE-LAB-[0-9]+","path":"/Users/fixture/secret-project"}});
    cases.push(custom);
    let mut pii = case(
        "privacy-opt-in-pii",
        vec![assistant(
            json!({"content":"fixture@example.invalid 203.0.113.42 415-555-2671"}),
        )],
    );
    pii.settings["redaction"] = json!({"pii":{"enabled":true}});
    cases.push(pii);
    let mut multiple = case(
        "multiple-turns-checkpoint-offsets",
        vec![assistant(json!({"content":"First turn."}))],
    );
    multiple.sessions[0].turns.push(turn(
        "Second edit.",
        vec![assistant(
            json!({"content":"Second turn.","messageId":"turn-2","outputTokens":5}),
        )],
    ));
    cases.push(multiple);
    let mut sessions = case(
        "multiple-sessions-one-checkpoint",
        vec![assistant(json!({"content":"Session one."}))],
    );
    sessions.sessions.push(Session {
        turns: vec![turn(
            "Second session.",
            vec![assistant(json!({"content":"Session two."}))],
        )],
        subagent_stops: vec![],
    });
    cases.push(sessions);
    let mut partial = case(
        "partial-commit-follow-up-checkpoint",
        vec![assistant(
            json!({"content":"Two files, two source commits."}),
        )],
    );
    partial.partial_commit = true;
    cases.push(partial);
    let mut multiline = case(
        "native-format-whitespace-escapes",
        vec![assistant(
            json!({"content":"\"quoted\" \\slash \u{0000} \t\r\ntrailing  \n","extra":{"created_at":"not-a-metadata-timestamp","cli_version":"literal-source-value"}}),
        )],
    );
    multiline.sessions[0].turns[0].prompt = "Prompt with trailing spaces.  \nSecond line.\n".into();
    multiline.sessions[0].turns[0].events[0]["data"]["content"] =
        json!(multiline.sessions[0].turns[0].prompt);
    cases.push(multiline);
    assert_eq!(
        cases.len(),
        50,
        "Schema corpus must contain exactly 50 fixtures"
    );
    cases
}
