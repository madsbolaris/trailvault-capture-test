use super::{Blob, Checkpoint, Run, digest};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn normalize(value: &mut Value, checkpoint: &Checkpoint) -> Result<()> {
    let version_pattern = regex::Regex::new(r"^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$")?;
    let object = value
        .as_object_mut()
        .context("Metadata/compact record must be an object")?;
    for (key, replacement) in [
        ("checkpoint_id", "<checkpoint-id>"),
        ("created_at", "<generated-time>"),
        ("cli_version", "<producer-version>"),
        ("turn_id", "<generated-turn-id>"),
    ] {
        let Some(field) = object.get_mut(key) else {
            continue;
        };
        let text = field
            .as_str()
            .with_context(|| format!("{key} must remain a string"))?;
        match key {
            "checkpoint_id" => {
                ensure!(
                    text == checkpoint.id,
                    "Metadata checkpoint ID does not match its ref"
                );
                text.parse::<ulid::Ulid>()
                    .context("Invalid checkpoint ULID")?;
            }
            "created_at" => {
                chrono::DateTime::parse_from_rfc3339(text)
                    .context("Invalid generated metadata timestamp")?;
            }
            "cli_version" => {
                ensure!(
                    version_pattern.is_match(text),
                    "Invalid producer version {text:?}"
                );
            }
            "turn_id" => {
                ensure!(
                    text.len() == 12
                        && text
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                    "Invalid generated turn ID {text:?}"
                );
            }
            _ => unreachable!(),
        }
        *field = json!(replacement);
    }
    Ok(())
}

fn parse_jsonl(bytes: &[u8]) -> Result<Vec<Value>> {
    ensure!(
        bytes.is_empty() || bytes.ends_with(b"\n"),
        "JSONL is not newline terminated"
    );
    std::str::from_utf8(bytes)?
        .lines()
        .enumerate()
        .map(|(index, line)| {
            ensure!(
                !line.trim().is_empty(),
                "Blank JSONL record at line {}",
                index + 1
            );
            let value: Value = serde_json::from_str(line)
                .with_context(|| format!("Invalid JSONL line {}", index + 1))?;
            ensure!(
                value.is_object(),
                "JSONL line {} is not an object",
                index + 1
            );
            Ok(value)
        })
        .collect()
}

fn mask_top_level_string(input: &str, key: &str, replacement: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut position = 0;
    let whitespace = |position: &mut usize| {
        while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
            *position += 1;
        }
    };
    whitespace(&mut position);
    ensure!(bytes.get(position) == Some(&b'{'), "Expected JSON object");
    position += 1;
    loop {
        whitespace(&mut position);
        if bytes.get(position) == Some(&b'}') {
            return Ok(input.into());
        }
        let start = position;
        ensure!(bytes.get(position) == Some(&b'"'), "Expected object key");
        position += 1;
        loop {
            match bytes.get(position).context("Unterminated object key")? {
                b'\\' => position += 2,
                b'"' => {
                    position += 1;
                    break;
                }
                _ => position += 1,
            }
        }
        let name: String = serde_json::from_str(&input[start..position])?;
        whitespace(&mut position);
        ensure!(bytes.get(position) == Some(&b':'), "Expected colon");
        position += 1;
        whitespace(&mut position);
        let start = position;
        let mut stream = serde_json::Deserializer::from_str(&input[start..]).into_iter::<Value>();
        let value = stream.next().context("Missing object value")??;
        position += stream.byte_offset();
        if name == key {
            ensure!(value.is_string(), "{key} must remain a string");
            return Ok(format!(
                "{}{}{}",
                &input[..start],
                serde_json::to_string(replacement)?,
                &input[position..]
            ));
        }
        whitespace(&mut position);
        match bytes.get(position) {
            Some(b',') => position += 1,
            Some(b'}') => return Ok(input.into()),
            _ => bail!("Expected comma or closing brace"),
        }
    }
}

fn compact_wire(blob: &Blob, checkpoint: &Checkpoint) -> Result<Value> {
    parse_jsonl(blob.bytes())?;
    let text = std::str::from_utf8(blob.bytes())?;
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        let value: Value = serde_json::from_str(line)?;
        let mut version = json!({});
        if let Some(value) = value.get("cli_version") {
            version["cli_version"] = value.clone();
            normalize(&mut version, checkpoint)?;
        }
        output.push_str(&mask_top_level_string(
            line,
            "cli_version",
            "<producer-version>",
        )?);
    }
    Ok(json!({"mode":blob.mode,"utf8":output}))
}

fn auxiliary_wire(path: &str, blob: &Blob) -> Result<Value> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 3 || parts[0] != "tasks" || parts[2] != "task.json" {
        return Ok(json!(blob));
    }
    let mut text = std::str::from_utf8(blob.bytes())?.to_owned();
    let value: Value = serde_json::from_str(&text)?;
    let mut times = BTreeMap::new();
    for key in ["started_at", "completed_at"] {
        if let Some(value) = value.get(key) {
            let time = chrono::DateTime::parse_from_rfc3339(
                value.as_str().context("Task timestamp must be a string")?,
            )?;
            times.insert(key, time);
            text = mask_top_level_string(&text, key, "<generated-task-time>")?;
        }
    }
    if let (Some(start), Some(end)) = (times.get("started_at"), times.get("completed_at")) {
        ensure!(start <= end, "Task completes before it starts");
    }
    Ok(json!({"mode":blob.mode,"utf8":text}))
}

#[derive(Clone, Copy)]
enum Shape {
    Keys,
    Types,
}

fn shape(value: &Value, mode: Shape) -> Value {
    match value {
        Value::Object(object) => json!(
            object
                .iter()
                .map(|(key, value)| (key.clone(), shape(value, mode)))
                .collect::<BTreeMap<_, _>>()
        ),
        Value::Array(array) => json!(
            array
                .iter()
                .map(|value| shape(value, mode))
                .collect::<Vec<_>>()
        ),
        _ => match mode {
            Shape::Keys => json!("<scalar>"),
            Shape::Types => json!(match value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::String(_) => "string",
                Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
                Value::Number(_) => "number",
                _ => unreachable!(),
            }),
        },
    }
}

fn difference(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    if expected == actual {
        return None;
    }
    match (expected, actual) {
        (Value::Object(left), Value::Object(right)) => {
            for key in left.keys().chain(right.keys()) {
                let path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (left.get(key), right.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(error) = difference(a, b, &path) {
                            return Some(error);
                        }
                    }
                    (Some(_), None) => return Some(format!("{path}: missing field/artifact")),
                    (None, Some(_)) => return Some(format!("{path}: unexpected field/artifact")),
                    _ => unreachable!(),
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                return Some(format!(
                    "{path}: expected {} records/items, got {}",
                    left.len(),
                    right.len()
                ));
            }
            for (index, (a, b)) in left.iter().zip(right).enumerate() {
                if let Some(error) = difference(a, b, &format!("{path}/{index}")) {
                    return Some(error);
                }
            }
        }
        _ => {}
    }
    Some(format!(
        "{path}: expected {}, got {}",
        concise(expected),
        concise(actual)
    ))
}

fn concise(value: &Value) -> String {
    let string = value.to_string();
    if string.chars().count() > 240 {
        format!("{}...", string.chars().take(240).collect::<String>())
    } else {
        string
    }
}

fn equal(expected: Value, actual: Value) -> Result<()> {
    if let Some(error) = difference(&expected, &actual, "") {
        bail!("{error}");
    }
    Ok(())
}

fn is_session_metadata(path: &str) -> bool {
    path.split_once('/').is_some_and(|(directory, file)| {
        directory.parse::<usize>().is_ok() && file == "metadata.json"
    })
}

fn is_base(path: &str) -> bool {
    path == "metadata.json"
        || path.split_once('/').is_some_and(|(directory, file)| {
            directory.parse::<usize>().is_ok()
                && matches!(
                    file,
                    "metadata.json"
                        | "full.jsonl"
                        | "transcript.jsonl"
                        | "prompt.txt"
                        | "content_hash.txt"
                )
        })
}

fn selected(run: &Run, index: usize) -> Result<Value> {
    let mut checkpoints = vec![];
    for checkpoint in &run.checkpoints {
        let mut output = BTreeMap::<String, Value>::new();
        for (path, blob) in &checkpoint.files {
            let selected = match index {
                0 => Some(json!(blob.mode)),
                1..=3 if path == "metadata.json" => Some(metadata(blob, checkpoint, index - 1)?),
                4..=6 if is_session_metadata(path) => Some(metadata(blob, checkpoint, index - 4)?),
                7..=9 if path.ends_with("/full.jsonl") => Some(match index {
                    7 => shape(&json!(parse_jsonl(blob.bytes())?), Shape::Types),
                    8 => json!(parse_jsonl(blob.bytes())?),
                    _ => json!(blob),
                }),
                10..=13 if path.ends_with("/transcript.jsonl") => {
                    let mut lines = parse_jsonl(blob.bytes())?;
                    for line in &mut lines {
                        // Compact producer versions are generated, but source
                        // timestamps and arbitrary nested payloads are not.
                        if line.get("cli_version").is_some() {
                            let mut version = json!({"cli_version":line["cli_version"]});
                            normalize(&mut version, checkpoint)?;
                            line["cli_version"] = version["cli_version"].clone();
                        }
                    }
                    Some(match index {
                        10 => compact_wire(blob, checkpoint)?,
                        11 => shape(&json!(lines), Shape::Keys),
                        12 => shape(&json!(lines), Shape::Types),
                        _ => json!(lines),
                    })
                }
                14 if path.ends_with("/prompt.txt") => Some(json!(blob)),
                15..=16 if path.ends_with("/content_hash.txt") => {
                    let bytes = blob.bytes();
                    let text = std::str::from_utf8(bytes)?;
                    if index == 15 {
                        Some(json!({
                            "prefix":text.starts_with("sha256:"),
                            "length":bytes.len(),
                            "lowercase_hex_digest":text.strip_prefix("sha256:").is_some_and(|hash| hash.as_bytes().get(..64).is_some_and(|prefix| prefix.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)))),
                            "newline":bytes.ends_with(b"\n"),
                        }))
                    } else {
                        let directory = path
                            .rsplit_once('/')
                            .context("Hash has no session directory")?
                            .0;
                        let full = checkpoint
                            .files
                            .get(&format!("{directory}/full.jsonl"))
                            .context("Hash has no native transcript")?;
                        ensure!(
                            text.trim_end_matches('\n')
                                == format!("sha256:{}", digest(full.bytes())),
                            "Content hash does not describe the stored native bytes"
                        );
                        Some(json!(blob))
                    }
                }
                17 if !is_base(path) => Some(if path.ends_with(".json") {
                    shape(
                        &serde_json::from_slice::<Value>(blob.bytes())?,
                        Shape::Types,
                    )
                } else if path.ends_with(".jsonl") {
                    shape(&json!(parse_jsonl(blob.bytes())?), Shape::Types)
                } else {
                    json!({"mode":blob.mode,"binary_or_text_bytes":blob.bytes().len()})
                }),
                18 if !is_base(path) => Some(auxiliary_wire(path, blob)?),
                _ => None,
            };
            if let Some(value) = selected {
                output.insert(path.clone(), value);
            }
        }
        checkpoints.push(json!(output));
    }
    Ok(json!(checkpoints))
}

fn metadata(blob: &Blob, checkpoint: &Checkpoint, mode: usize) -> Result<Value> {
    let mut value: Value = serde_json::from_slice(blob.bytes())?;
    normalize(&mut value, checkpoint)?;
    Ok(match mode {
        0 => shape(&value, Shape::Keys),
        1 => shape(&value, Shape::Types),
        2 => value,
        _ => unreachable!(),
    })
}

fn validate_references(run: &Run) -> Result<Value> {
    ensure!(!run.checkpoints.is_empty(), "No checkpoint artifacts");
    let mut boundaries = vec![];
    let mut turn_aliases = BTreeMap::new();
    let mut checkpoint_aliases = BTreeMap::new();
    for checkpoint in &run.checkpoints {
        checkpoint
            .id
            .parse::<ulid::Ulid>()
            .context("Invalid checkpoint ref ID")?;
        let root = checkpoint
            .files
            .get("metadata.json")
            .context("Missing root metadata")?;
        let root: Value = serde_json::from_slice(root.bytes())?;
        ensure!(
            root["checkpoint_id"] == checkpoint.id,
            "Root checkpoint ID differs from source trailer"
        );
        let sessions = root["sessions"]
            .as_array()
            .context("Root metadata has no sessions array")?;
        ensure!(!sessions.is_empty(), "No sessions in checkpoint");
        let mut offsets = BTreeMap::new();
        for (path, blob) in &checkpoint.files {
            if is_session_metadata(path) {
                let metadata: Value = serde_json::from_slice(blob.bytes())?;
                let mut fields = BTreeMap::new();
                for key in [
                    "session_id",
                    "checkpoint_transcript_start",
                    "checkpoint_transcript_end",
                    "compact_transcript_start",
                    "transcript_start",
                    "transcript_end",
                    "is_partial",
                ] {
                    if let Some(value) = metadata.get(key) {
                        fields.insert(key, value.clone());
                    }
                }
                if let Some(turn) = metadata.get("turn_id") {
                    let turn = turn.as_str().context("Turn ID is not a string")?;
                    let next = turn_aliases.len();
                    let alias = turn_aliases.entry(turn.to_owned()).or_insert(next);
                    fields.insert("turn_identity", json!(*alias));
                }
                offsets.insert(path.clone(), fields);
            }
        }
        fn references(value: &Value, checkpoint: &Checkpoint) -> Result<()> {
            match value {
                Value::String(path) if path.starts_with('/') => {
                    ensure!(
                        checkpoint.files.contains_key(&path[1..]),
                        "Dangling checkpoint artifact reference {path}"
                    );
                }
                Value::Object(object) => {
                    for value in object.values() {
                        references(value, checkpoint)?;
                    }
                }
                Value::Array(array) => {
                    for value in array {
                        references(value, checkpoint)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        references(&root["sessions"], checkpoint)?;
        let session_count = checkpoint
            .files
            .keys()
            .filter(|path| is_session_metadata(path))
            .count();
        ensure!(
            sessions.len() == session_count,
            "Session index does not match session metadata files"
        );
        for (path, blob) in &checkpoint.files {
            if path.starts_with("tasks/") && path.ends_with("/task.json") {
                let task: Value = serde_json::from_slice(blob.bytes())?;
                let id = task["tool_use_id"]
                    .as_str()
                    .context("Task has no tool_use_id")?;
                ensure!(
                    path == &format!("tasks/{id}/task.json"),
                    "Task path and tool-use identity differ"
                );
                let agent = task["agent_id"].as_str().context("Task has no agent_id")?;
                let mut linked = false;
                for (file, blob) in &checkpoint.files {
                    if file.ends_with("/transcript.jsonl") {
                        for line in parse_jsonl(blob.bytes())? {
                            if let Some(blocks) = line["content"].as_array() {
                                linked |= blocks.iter().any(|block| {
                                    block["type"] == "tool_use"
                                        && block["id"] == id
                                        && block["result"]["agent"]["id"] == agent
                                });
                            }
                        }
                    }
                }
                ensure!(
                    linked,
                    "Task {id} is not linked to its compact tool block and child agent"
                );
            }
        }
        let next = checkpoint_aliases.len();
        let alias = *checkpoint_aliases
            .entry(checkpoint.id.clone())
            .or_insert(next);
        boundaries.push(
            json!({"checkpoint_identity":alias,"session_count":session_count,"offsets":offsets}),
        );
    }
    Ok(json!({"checkpoints":boundaries,"native_source_digests":run.native}))
}

pub fn check(index: usize, expected: &Run, actual: &Run) -> Result<()> {
    ensure!(index < super::CHECKS.len(), "Unknown schema selector");
    if index == 19 {
        equal(validate_references(expected)?, validate_references(actual)?)
    } else {
        equal(selected(expected, index)?, selected(actual, index)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_extra_and_renamed_fields() {
        for changed in [
            json!({"a":1}),
            json!({"a":1,"b":2,"c":3}),
            json!({"a":1,"renamed":2}),
        ] {
            assert!(
                equal(
                    shape(&json!({"a":1,"b":2}), Shape::Keys),
                    shape(&changed, Shape::Keys)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn distinguishes_every_json_scalar_type_and_integer_numbers() {
        let values = [Value::Null, json!(false), json!("1"), json!(1), json!(1.5)];
        for (i, left) in values.iter().enumerate() {
            for (j, right) in values.iter().enumerate() {
                assert_eq!(
                    equal(shape(left, Shape::Types), shape(right, Shape::Types)).is_ok(),
                    i == j
                );
            }
        }
    }

    #[test]
    fn distinguishes_absent_null_empty_and_false() {
        let values = [
            json!({}),
            json!({"a":null}),
            json!({"a":""}),
            json!({"a":false}),
            json!({"a":[]}),
        ];
        for (i, left) in values.iter().enumerate() {
            for (j, right) in values.iter().enumerate() {
                assert_eq!(equal(left.clone(), right.clone()).is_ok(), i == j);
            }
        }
    }

    #[test]
    fn detects_nested_changes_and_array_reordering() {
        let expected = json!({"content":[{"id":"a","input":{"x":1}},{"id":"b"}]});
        for changed in [
            json!({"content":[{"id":"a","input":{"x":2}},{"id":"b"}]}),
            json!({"content":[{"id":"b"},{"id":"a","input":{"x":1}}]}),
            json!({"content":[{"id":"a","input":{"x":1}}]}),
        ] {
            assert!(equal(expected.clone(), changed).is_err());
        }
    }

    #[test]
    fn jsonl_rejects_missing_newline_blank_records_and_nonobjects() {
        for bytes in [b"{}".as_slice(), b"{}\n\n", b"[]\n", b"null\n", b"{\n"] {
            assert!(parse_jsonl(bytes).is_err(), "{bytes:?}");
        }
        assert_eq!(
            parse_jsonl(b"  {\"x\":1} \r\n").unwrap(),
            vec![json!({"x":1})]
        );
    }

    #[test]
    fn binary_and_final_newline_differences_are_not_lost() {
        for (left, right) in [
            (vec![0, 255, 1], vec![0, 254, 1]),
            (b"x\n".to_vec(), b"x".to_vec()),
        ] {
            assert!(
                equal(
                    json!(Blob::new("100644", left)),
                    json!(Blob::new("100644", right))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn normalization_is_typed_allowlisted_and_not_recursive() {
        let checkpoint = Checkpoint {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            files: BTreeMap::new(),
        };
        let nested =
            json!({"created_at":"literal","cli_version":"literal","checkpoint_id":"literal"});
        let mut value = json!({"checkpoint_id":checkpoint.id,"created_at":"2026-01-02T03:04:05Z","cli_version":"0.12.2","content":nested});
        normalize(&mut value, &checkpoint).unwrap();
        assert_eq!(value["content"], nested);
        assert_eq!(value["checkpoint_id"], "<checkpoint-id>");
        for mut bad in [
            json!({"created_at":null}),
            json!({"created_at":"yesterday"}),
            json!({"cli_version":12}),
            json!({"cli_version":""}),
            json!({"checkpoint_id":"wrong"}),
            json!({"turn_id":"arbitrary"}),
        ] {
            assert!(normalize(&mut bad, &checkpoint).is_err());
        }
        let mut absent = json!({});
        normalize(&mut absent, &checkpoint).unwrap();
        assert_eq!(absent, json!({}));
    }

    #[test]
    fn corpus_has_fifty_distinct_nonidentical_fixtures() {
        let cases = super::super::corpus::cases();
        let names: std::collections::BTreeSet<_> = cases.iter().map(|case| &case.name).collect();
        let payloads: std::collections::BTreeSet<_> = cases.iter().map(|case| serde_json::to_string(&json!({"sessions":case.sessions,"settings":case.settings,"partial_commit":case.partial_commit})).unwrap()).collect();
        assert_eq!(names.len(), 50);
        assert_eq!(payloads.len(), 50);
        assert_eq!(cases.len() * super::super::CHECKS.len(), 1000);
    }

    #[test]
    fn wire_normalization_preserves_nested_literals_format_and_record_order() {
        let text = " { \"content\":{\"cli_version\":\"literal\",\"started_at\":\"literal\"}, \"cli_version\" : \"0.12.2\", \"tail\":true }\r\n";
        let masked = mask_top_level_string(text, "cli_version", "<version>").unwrap();
        assert_eq!(masked, text.replace("\"0.12.2\"", "\"<version>\""));
        assert_eq!(
            mask_top_level_string(text, "absent", "unused").unwrap(),
            text
        );
        assert!(mask_top_level_string("{\"cli_version\":null}", "cli_version", "unused").is_err());
        let task = Blob::new("100644",b"{\"started_at\":\"2026-01-02T03:04:05Z\",\"completed_at\":\"2026-01-02T03:04:06Z\",\"description\":{\"started_at\":\"literal\"}}\n".to_vec());
        let normalized = auxiliary_wire("tasks/id/task.json", &task).unwrap();
        assert!(
            normalized["utf8"]
                .as_str()
                .unwrap()
                .contains("\"started_at\":\"literal\"")
        );
        assert_eq!(auxiliary_wire("0/other.json", &task).unwrap(), json!(task));
    }

    #[test]
    fn every_selector_detects_artifact_mutations() {
        let cases = super::super::corpus::cases();
        let reference = super::super::load(&cases).unwrap();
        for (index, path) in [
            (0, "metadata.json"),
            (1, "metadata.json"),
            (2, "metadata.json"),
            (3, "metadata.json"),
            (4, "0/metadata.json"),
            (5, "0/metadata.json"),
            (6, "0/metadata.json"),
            (7, "0/full.jsonl"),
            (8, "0/full.jsonl"),
            (9, "0/full.jsonl"),
            (10, "0/transcript.jsonl"),
            (11, "0/transcript.jsonl"),
            (12, "0/transcript.jsonl"),
            (13, "0/transcript.jsonl"),
            (14, "0/prompt.txt"),
            (15, "0/content_hash.txt"),
            (16, "0/content_hash.txt"),
            (19, "metadata.json"),
        ] {
            let mut changed = reference[0].clone();
            changed.checkpoints[0].files.remove(path);
            assert!(
                check(index, &reference[0], &changed).is_err(),
                "Selector {index} missed removed {path}"
            );
        }
        let task_index = cases
            .iter()
            .position(|case| case.name == "subagent-durable-task")
            .unwrap();
        for index in [17, 18] {
            let mut changed = reference[task_index].clone();
            changed.checkpoints[0]
                .files
                .remove("tasks/task-0/task.json");
            assert!(check(index, &reference[task_index], &changed).is_err());
        }
        let mut changed = reference[0].clone();
        changed.checkpoints[0]
            .files
            .get_mut("0/full.jsonl")
            .unwrap()
            .mode = "100755".into();
        assert!(check(0, &reference[0], &changed).is_err());
    }

    #[test]
    fn wire_checks_detect_formatting_and_relationship_checks_detect_dangling_tasks() {
        let cases = super::super::corpus::cases();
        let reference = super::super::load(&cases).unwrap();
        let mut changed = reference[0].clone();
        let blob = changed.checkpoints[0]
            .files
            .get_mut("0/transcript.jsonl")
            .unwrap();
        let mut bytes = vec![b' '];
        bytes.extend_from_slice(blob.bytes());
        *blob = Blob::new("100644", bytes);
        assert!(check(10, &reference[0], &changed).is_err());
        assert!(check(13, &reference[0], &changed).is_ok());
        let index = cases
            .iter()
            .position(|case| case.name == "subagent-durable-task")
            .unwrap();
        let mut changed = reference[index].clone();
        let blob = changed.checkpoints[0]
            .files
            .get_mut("tasks/task-0/task.json")
            .unwrap();
        let mut value: Value = serde_json::from_slice(blob.bytes()).unwrap();
        value["agent_id"] = json!("unlinked-agent");
        *blob = Blob::new("100644", serde_json::to_vec(&value).unwrap());
        assert!(check(19, &reference[index], &changed).is_err());
    }
}
