use crate::{
    capture,
    git::{self, Repo},
    redact,
};
use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)https://github\.com/([a-z0-9_.-]+/[a-z0-9_.-]+)/pull/([1-9][0-9]*)(?:[/?#]|$|[)\]}>.,;!'"`\s])"#).expect("PR URL regex")
});
static QUALIFIED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([a-z0-9_.-]+/[a-z0-9_.-]+)#([1-9][0-9]*)\b").expect("qualified PR regex")
});
static SHORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s(\[])#([1-9][0-9]*)\b").expect("short PR regex"));
static BARE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)/([a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9_.-]*)/pull/([1-9][0-9]*)/?")
        .expect("contextual PR path regex")
});
static GH_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^(?:([a-zA-Z0-9_./:=,#?+-]+)|'([a-zA-Z0-9_./:=,#?+ -]*)'|"([a-zA-Z0-9_./:=,#?+ -]*)")"#,
    )
    .expect("literal GitHub CLI argument regex")
});

fn bare_path_boundaries(text: &str, matched: regex::Match<'_>) -> bool {
    if text[..matched.start()]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && !"([<{\"'`".contains(c))
    {
        return false;
    }
    let closing = |c: char| c.is_whitespace() || ")]}>\"'`".contains(c);
    let mut suffix = text[matched.end()..].chars();
    match suffix.next() {
        None => true,
        Some(c) if closing(c) => true,
        Some(c) if ".,;!:?".contains(c) => suffix.next().is_none_or(closing),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub host: String,
    pub repository: String,
    pub number: u64,
}
impl Target {
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/pull/{}",
            self.repository, self.number
        )
    }
    pub fn parse(url: &str) -> Result<Self> {
        let matches = mentions(url, None)?;
        ensure!(
            matches.len() == 1
                && url.starts_with("https://github.com/")
                && !url
                    .chars()
                    .any(|c| c.is_whitespace() || "\"<>`".contains(c)),
            "Expected one github.com pull request URL"
        );
        let lower = url.to_ascii_lowercase();
        let suffix = lower
            .strip_prefix(&matches[0].url())
            .context("Expected a complete pull request URL")?;
        let path = suffix.split(['?', '#']).next().unwrap_or_default();
        ensure!(
            ["", "/", "/files", "/commits", "/checks"].contains(&path),
            "Invalid pull request URL suffix"
        );
        Ok(matches[0].clone())
    }
}

fn repository(value: &str) -> Result<String> {
    let parts: Vec<_> = value.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|p| !p.is_empty()
                && ![".", ".."].contains(p)
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
        "Invalid PR repository identity"
    );
    Ok(value.to_ascii_lowercase())
}

pub fn origin_repository(repo: &Repo) -> Result<Option<String>> {
    if !repo.g(&["remote"])?.lines().any(|name| name == "origin") {
        return Ok(None);
    }
    let origin = repo.g(&["remote", "get-url", "origin"])?;
    let path = origin
        .strip_prefix("https://github.com/")
        .or_else(|| origin.strip_prefix("git@github.com:"))
        .or_else(|| origin.strip_prefix("ssh://git@github.com/"));
    path.map(|path| {
        let path = path.trim_end_matches('/');
        repository(path.strip_suffix(".git").unwrap_or(path))
    })
    .transpose()
}

pub fn mentions(text: &str, context: Option<&str>) -> Result<Vec<Target>> {
    let mut found = BTreeSet::new();
    for (expression, qualified) in [(&*URL, false), (&*QUALIFIED, true)] {
        for matched in expression.captures_iter(text) {
            if qualified
                && matched.get(0).is_some_and(|m| {
                    text[..m.start()]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
                })
            {
                continue;
            }
            found.insert(Target {
                host: "github.com".into(),
                repository: repository(&matched[1])?,
                number: matched[2]
                    .parse()
                    .context("PR number exceeds supported range")?,
            });
            ensure!(
                found.len() <= 256,
                "Too many distinct PR references in one record"
            );
        }
    }
    if let Some(context) = context {
        let context = repository(context)?;
        for matched in BARE_PATH.captures_iter(text) {
            if repository(&matched[1])? != context
                || !bare_path_boundaries(text, matched.get(0).expect("matched PR path"))
            {
                continue;
            }
            found.insert(Target {
                host: "github.com".into(),
                repository: context.clone(),
                number: matched[2]
                    .parse()
                    .context("PR number exceeds supported range")?,
            });
            ensure!(
                found.len() <= 256,
                "Too many distinct PR references in one record"
            );
        }
        for matched in SHORT.captures_iter(text) {
            found.insert(Target {
                host: "github.com".into(),
                repository: context.clone(),
                number: matched[1]
                    .parse()
                    .context("PR number exceeds supported range")?,
            });
            ensure!(
                found.len() <= 256,
                "Too many distinct PR references in one record"
            );
        }
    }
    Ok(found.into_iter().collect())
}

fn gh_lookup(command: &str, context: Option<&str>) -> Result<Option<Vec<Target>>> {
    let mut rest = command;
    let mut words = vec![];
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\r']);
        if rest.is_empty() || rest.starts_with([';', '&', '|', '\n']) {
            break;
        }
        let Some(word) = GH_WORD.captures(rest) else {
            return Ok(None);
        };
        let value = word
            .iter()
            .skip(1)
            .flatten()
            .next()
            .expect("literal argument");
        if words.is_empty() && value.as_str() != "gh" {
            return Ok(None);
        }
        words.push(value.as_str());
        ensure!(words.len() <= 256, "Too many GitHub PR lookup arguments");
        rest = &rest[word.get(0).expect("matched argument").end()..];
        if !rest.is_empty() && !rest.starts_with([' ', '\t', '\r', '\n', ';', '&', '|']) {
            return Ok(None);
        }
    }
    if words.len() < 3 || words[1] != "pr" || !["view", "diff", "checks"].contains(&words[2]) {
        return Ok(None);
    }
    let mut repo = context.map(repository).transpose()?;
    let mut selector = None;
    let mut args = words[3..].iter().copied();
    while let Some(arg) = args.next() {
        if arg == "--repo" || arg == "-R" || arg.starts_with("--repo=") {
            let Some(value) = arg.strip_prefix("--repo=").or_else(|| args.next()) else {
                return Ok(None);
            };
            let explicit_host = value.strip_prefix("github.com/");
            let value = explicit_host.unwrap_or(value);
            if value.split('/').count() != 2 {
                return Ok(None);
            }
            repo = if context.is_some() || explicit_host.is_some() {
                Some(repository(value)?)
            } else {
                None
            };
        } else if [
            "--json",
            "--jq",
            "-q",
            "--template",
            "-t",
            "--interval",
            "-i",
        ]
        .contains(&arg)
        {
            if args.next().is_none() {
                return Ok(None);
            }
        } else if [
            "--comments",
            "--web",
            "--watch",
            "--required",
            "--exit-status",
        ]
        .contains(&arg)
            || ["--json=", "--jq=", "--template=", "--interval="]
                .iter()
                .any(|prefix| arg.starts_with(prefix))
        {
        } else if arg.starts_with('-') || selector.replace(arg).is_some() {
            return Ok(None);
        }
    }
    let mut targets = vec![];
    if let Some(selector) = selector {
        if selector.starts_with("https://github.com/") {
            let target = Target::parse(selector)?;
            targets.push(target);
        } else if selector.contains("://") || selector.starts_with('/') {
            return Ok(None);
        } else if selector.bytes().all(|c| c.is_ascii_digit()) && !selector.starts_with('0') {
            if let Some(repository) = repo {
                targets.push(Target {
                    host: "github.com".into(),
                    repository,
                    number: selector
                        .parse()
                        .context("PR number exceeds supported range")?,
                });
            }
        }
    }
    Ok(Some(targets))
}

pub(crate) fn reference_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(reference_text)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(values) => values
            .values()
            .map(reference_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub kind: String,
    pub role: String,
    pub record: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub target: Target,
    pub url: String,
    pub relationship: String,
    pub retrieval_verified: bool,
    pub contribution_verified: bool,
    pub evidence: Vec<Evidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    pub version: u8,
    pub context_repository: Option<String>,
    pub relationships: Vec<Relationship>,
}

impl Collection {
    pub fn accumulate(&mut self, incoming: Self, source: Option<&str>) -> Result<()> {
        ensure!(
            self.version == 1 && incoming.version == 1,
            "Unsupported relationship version"
        );
        ensure!(
            self.context_repository == incoming.context_repository,
            "Relationship context changed"
        );
        let mut found: BTreeMap<_, _> = self
            .relationships
            .iter()
            .cloned()
            .map(|relationship| (relationship.target.clone(), relationship))
            .collect();
        let rank = |label: &str| match label {
            "provider_linked" => 3,
            "review_requested" => 2,
            "read_requested" => 1,
            _ => 0,
        };
        for mut relationship in incoming.relationships {
            if let Some(source) = source {
                for evidence in &mut relationship.evidence {
                    evidence.source = Some(source.into());
                }
            }
            if let Some(existing) = found.get_mut(&relationship.target) {
                if rank(&relationship.relationship) > rank(&existing.relationship) {
                    existing.relationship = relationship.relationship;
                }
                for evidence in relationship.evidence {
                    if !existing.evidence.contains(&evidence) {
                        existing.evidence.push(evidence);
                    }
                }
            } else {
                found.insert(relationship.target.clone(), relationship);
            }
            ensure!(found.len() <= 256, "Too many accumulated PR relationships");
        }
        self.relationships = found.into_values().collect();
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Edited,
    Reviewed,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityEvidence {
    pub activity: Activity,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionActivity {
    pub target: Target,
    pub activities: Vec<Activity>,
    pub evidence: Vec<ActivityEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCollection {
    pub version: u8,
    pub relationships: Vec<SessionActivity>,
}

fn failed_result(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.get("success") == Some(&json!(false))
                || object.get("isError") == Some(&json!(true))
                || object.get("is_error") == Some(&json!(true))
                || ["exit_code", "exitCode"].iter().any(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_i64)
                        .is_some_and(|c| c != 0)
                })
                || object
                    .get("error")
                    .is_some_and(|error| !error.is_null() && error != &json!(false))
                || object.values().any(failed_result)
        }
        Value::Array(values) => values.iter().any(failed_result),
        Value::String(text) if text.trim_start().starts_with(['{', '[']) => {
            serde_json::from_str::<Value>(text).is_ok_and(|value| failed_result(&value))
        }
        _ => false,
    }
}

fn feedback_operation(start: &Value, target: &Target, context: Option<&str>) -> bool {
    let name = start["toolName"].as_str().unwrap_or_default();
    let arguments = if let Some(text) = start["arguments"].as_str() {
        serde_json::from_str::<Value>(text).ok()
    } else {
        Some(start["arguments"].clone())
    };
    let Some(arguments) = arguments else {
        return false;
    };
    let comment = ["add_issue_comment", "add_pull_request_review_comment"]
        .iter()
        .any(|operation| name.ends_with(operation));
    let review = (name.ends_with("pull_request_review_write")
        && matches!(arguments["method"].as_str(), Some("create" | "submit")))
        || name.ends_with("submit_pull_request_review")
        || name.ends_with("create_pull_request_review");
    if comment || review {
        let number = [
            "pullNumber",
            "pull_number",
            "issue_number",
            "issueNumber",
            "number",
        ]
        .iter()
        .find_map(|key| arguments[*key].as_u64());
        return arguments["owner"]
            .as_str()
            .zip(arguments["repo"].as_str())
            .is_some_and(|(owner, repo)| {
                format!("{owner}/{repo}").eq_ignore_ascii_case(&target.repository)
            })
            && number == Some(target.number)
            && (!review
                || matches!(
                    arguments["event"].as_str(),
                    Some("APPROVE" | "COMMENT" | "REQUEST_CHANGES")
                ));
    }

    if ![
        "bash",
        "Bash",
        "exec_command",
        "shell_command",
        "shell",
        "commandExecution",
    ]
    .iter()
    .any(|tool| name == *tool || name.ends_with(&format!(".{tool}")))
    {
        return false;
    }
    let Some(command) = arguments
        .get("command")
        .or_else(|| arguments.get("cmd"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    // A shell wrapper returning successfully does not prove a nested write ran.
    if command.contains([';', '|', '&', '\n', '`', '$', '<', '>']) {
        return false;
    }
    let words: Vec<_> = command
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"']))
        .collect();
    if words.first() == Some(&"gh") && words.get(1) == Some(&"api") {
        let endpoint = if words
            .get(2)
            .is_some_and(|word| ["-X", "--method"].contains(word))
        {
            words.get(4)
        } else {
            words.get(2)
        };
        let Some(endpoint) = endpoint.map(|endpoint| endpoint.trim_start_matches('/')) else {
            return false;
        };
        let root = format!("repos/{}", target.repository);
        let comment = [
            format!("{root}/issues/{}/comments", target.number),
            format!("{root}/pulls/{}/comments", target.number),
        ]
        .contains(&endpoint.to_owned());
        let review = endpoint == format!("{root}/pulls/{}/reviews", target.number)
            && words.iter().any(|word| {
                ["event=COMMENT", "event=APPROVE", "event=REQUEST_CHANGES"].contains(word)
            });
        return (comment || review)
            && words
                .windows(2)
                .any(|pair| ["--method", "-X"].contains(&pair[0]) && pair[1] == "POST");
    }
    if words.len() < 4
        || words[..2] != ["gh", "pr"]
        || !["comment", "review"].contains(&words[2])
        || words.iter().any(|word| ["--help", "-h"].contains(word))
    {
        return false;
    }
    let supplied_repo = words
        .windows(2)
        .find_map(|pair| ["--repo", "-R"].contains(&pair[0]).then_some(pair[1]));
    if supplied_repo.is_some_and(|repo| !repo.eq_ignore_ascii_case(&target.repository)) {
        return false;
    }
    words[3] == target.url()
        || (words[3] == target.number.to_string()
            && supplied_repo
                .or(context)
                .is_some_and(|repo| repo.eq_ignore_ascii_case(&target.repository)))
}

fn read_operation(start: &Value, target: &Target, context: Option<&str>) -> bool {
    let name = start["toolName"].as_str().unwrap_or_default();
    let args = &start["arguments"];
    if name.ends_with("pull_request_read") {
        return matches!(
            args["method"].as_str(),
            Some("get" | "get_diff" | "get_files" | "get_comments" | "get_reviews")
        ) && args["owner"]
            .as_str()
            .zip(args["repo"].as_str())
            .is_some_and(|(owner, repo)| {
                format!("{owner}/{repo}").eq_ignore_ascii_case(&target.repository)
            })
            && args["pullNumber"].as_u64() == Some(target.number);
    }
    if !matches!(
        name,
        "bash" | "Bash" | "exec_command" | "shell_command" | "shell" | "commandExecution"
    ) {
        return false;
    }
    let Some(command) = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    if command.contains([';', '|', '&', '\n', '`', '$', '<', '>']) {
        return false;
    }
    let words: Vec<_> = command
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"']))
        .collect();
    if words.first() == Some(&"conversation-history.mjs") {
        return words
            .get(1)
            .is_some_and(|repo| repo.eq_ignore_ascii_case(&target.repository))
            && words
                .get(2)
                .is_some_and(|number| *number == target.number.to_string())
            && words.get(3) == Some(&"read");
    }
    let supplied_repo = words
        .windows(2)
        .find_map(|pair| ["--repo", "-R"].contains(&pair[0]).then_some(pair[1]));
    words.len() >= 4
        && words[..2] == ["gh", "pr"]
        && ["view", "diff"].contains(&words[2])
        && !words.iter().any(|word| ["--help", "-h"].contains(word))
        && supplied_repo.is_none_or(|repo| repo.eq_ignore_ascii_case(&target.repository))
        && (words[3] == target.url()
            || (words[3] == target.number.to_string()
                && supplied_repo
                    .or(context)
                    .is_some_and(|repo| repo.eq_ignore_ascii_case(&target.repository))))
}

pub fn validate_activities(stored: &Value, actual: &ActivityCollection) -> Result<()> {
    let mut previous: ActivityCollection = serde_json::from_value(stored.clone())?;
    // Older snapshots included empty rows for links retained only in canonical history.
    previous.relationships.retain(|old| {
        !old.activities.is_empty()
            || !old.evidence.is_empty()
            || actual
                .relationships
                .iter()
                .any(|current| current.target == old.target)
    });
    let mut expected = actual.clone();
    for (old, current) in previous
        .relationships
        .iter()
        .zip(&mut expected.relationships)
    {
        if old.target == current.target
            && old.activities == [Activity::Read]
            && old.evidence.len() == 1
            && old.evidence[0].kind == "generic_related_session_fallback_not_retrieval_proof"
            && old.evidence[0].activity == Activity::Read
            && old.evidence[0].record.is_none()
            && (current.activities.is_empty() || current.activities == [Activity::Read])
        {
            *current = old.clone();
        }
    }
    ensure!(
        previous == expected,
        "Stored PR activities do not match captured evidence"
    );
    Ok(())
}

fn file_mutation(start: &Value, result: &Value) -> bool {
    let name = start["toolName"].as_str().unwrap_or_default();
    let arguments = &start["arguments"];
    let output = reference_text(result);
    if name.ends_with("apply_patch") {
        let patch = arguments
            .get("input")
            .or_else(|| arguments.get("patch"))
            .and_then(Value::as_str)
            .or_else(|| arguments.as_str());
        return patch.is_some_and(|patch| {
            patch.starts_with("*** Begin Patch")
                && patch.trim_end().ends_with("*** End Patch")
                && ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                    .iter()
                    .any(|marker| patch.contains(marker))
        }) && output.contains("Success. Updated the following files:");
    }
    let path = arguments.get("file_path").or_else(|| arguments.get("path"));
    if !path
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty())
    {
        return false;
    }
    let output = output.to_ascii_lowercase();
    let confirmed = [
        "updated successfully",
        "created successfully",
        "overwritten successfully",
        "successfully edited",
        "successfully replaced",
    ]
    .iter()
    .any(|message| output.contains(message));
    if !confirmed {
        return false;
    }
    let replacement = |arguments: &Value| {
        arguments["old_string"]
            .as_str()
            .zip(arguments["new_string"].as_str())
            .or_else(|| {
                arguments["old_str"]
                    .as_str()
                    .zip(arguments["new_str"].as_str())
            })
            .is_some_and(|(old, new)| old != new)
    };
    match name.rsplit('.').next() {
        Some("Edit" | "edit" | "str_replace") => replacement(arguments),
        Some("MultiEdit") => arguments["edits"]
            .as_array()
            .is_some_and(|edits| edits.iter().any(replacement)),
        Some("Write" | "write_file" | "create") => arguments["content"].is_string(),
        _ => false,
    }
}

pub fn activities(
    compact: &str,
    links: &Collection,
    files_touched: &[String],
) -> Result<ActivityCollection> {
    ensure!(links.version == 1, "Unsupported PR relationship version");
    let records = redact::parse(compact)?;
    ensure!(
        records
            .first()
            .is_some_and(|header| header["type"] == "header"
                && matches!(header["version"].as_u64(), Some(1 | 2))),
        "Unsupported compact transcript for PR activities"
    );
    let mut pending = BTreeMap::new();
    let mut posted = BTreeMap::<Target, Vec<usize>>::new();
    let mut reads = BTreeMap::<Target, Vec<usize>>::new();
    let mut edits = vec![];
    for (record, row) in records.iter().enumerate() {
        let data = &row["data"];
        let Some(id) = data["toolCallId"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        match row["type"].as_str() {
            Some("tool.execution_start") => {
                ensure!(
                    pending.insert(id, data).is_none(),
                    "Duplicate PR activity tool-call identity"
                );
            }
            Some("tool.execution_complete") => {
                let Some(start) = pending.remove(id) else {
                    continue;
                };
                if data["success"] != true || failed_result(data) {
                    continue;
                }
                if file_mutation(start, &data["result"]) {
                    edits.push(record);
                }
                let output = reference_text(&data["result"]);
                for link in &links.relationships {
                    let target = &link.target;
                    if !output.trim().is_empty()
                        && read_operation(start, target, links.context_repository.as_deref())
                    {
                        reads.entry(target.clone()).or_default().push(record);
                    }
                    let posted_url = ["#issuecomment-", "#pullrequestreview-", "#discussion_r"]
                        .iter()
                        .any(|anchor| output.contains(&format!("{}{anchor}", target.url())));
                    if posted_url
                        && feedback_operation(start, target, links.context_repository.as_deref())
                    {
                        posted.entry(target.clone()).or_default().push(record);
                    }
                }
            }
            _ => {}
        }
    }
    let relationships = links
        .relationships
        .iter()
        .map(|link| {
            let mut activities = vec![];
            let mut evidence = vec![];
            if !files_touched.is_empty() || !edits.is_empty() {
                activities.push(Activity::Edited);
                if !files_touched.is_empty() {
                    evidence.push(ActivityEvidence {
                        activity: Activity::Edited,
                        kind: "observed_file_changes".into(),
                        record: None,
                    });
                }
                evidence.extend(edits.iter().map(|record| ActivityEvidence {
                    activity: Activity::Edited,
                    kind: "successful_file_mutation".into(),
                    record: Some(*record),
                }));
            }
            if let Some(records) = posted.get(&link.target) {
                activities.push(Activity::Reviewed);
                evidence.extend(records.iter().map(|record| ActivityEvidence {
                    activity: Activity::Reviewed,
                    kind: "successful_feedback_write_with_posted_url".into(),
                    record: Some(*record),
                }));
            }
            if activities.is_empty() && reads.contains_key(&link.target) {
                activities.push(Activity::Read);
                evidence.extend(reads[&link.target].iter().map(|record| ActivityEvidence {
                    activity: Activity::Read,
                    kind: "successful_pr_retrieval".into(),
                    record: Some(*record),
                }));
            }
            SessionActivity {
                target: link.target.clone(),
                activities,
                evidence,
            }
        })
        .collect();
    Ok(ActivityCollection {
        version: 1,
        relationships,
    })
}

pub fn collect(
    compact: &str,
    context: Option<&str>,
    provider_links: Option<&Value>,
) -> Result<Collection> {
    let mut found: BTreeMap<Target, Relationship> = BTreeMap::new();
    let mut add = |text: &str, role: &str, record: usize, linked: bool| -> Result<()> {
        for target in mentions(text, context)? {
            let entry = found.entry(target.clone()).or_insert_with(|| Relationship {
                url: target.url(),
                target,
                relationship: "mentioned".into(),
                retrieval_verified: false,
                contribution_verified: false,
                evidence: vec![],
            });
            if linked {
                entry.relationship = "provider_linked".into();
            }
            let mut kind = if linked { "provider_link" } else { "mention" };
            if role == "user" {
                let lower = text.to_ascii_lowercase();
                let request = lower.strip_prefix("please ").unwrap_or(&lower);
                for (verb, label) in [
                    ("review ", "review_requested"),
                    ("read ", "read_requested"),
                    ("view ", "read_requested"),
                    ("inspect ", "read_requested"),
                ] {
                    if let Some(rest) = request.strip_prefix(verb) {
                        let rest = rest
                            .strip_prefix("this ")
                            .or_else(|| rest.strip_prefix("the "))
                            .unwrap_or(rest);
                        let rest = rest
                            .strip_prefix("pull request ")
                            .or_else(|| rest.strip_prefix("pr "))
                            .unwrap_or(rest);
                        let mut forms = vec![
                            entry.target.url(),
                            format!("{}#{}", entry.target.repository, entry.target.number),
                        ];
                        if context.is_some_and(|c| c.eq_ignore_ascii_case(&entry.target.repository))
                        {
                            forms.push(format!("#{}", entry.target.number));
                        }
                        if forms.iter().any(|form| {
                            rest.strip_prefix(form).is_some_and(|suffix| {
                                suffix.is_empty()
                                    || suffix.starts_with(|c: char| {
                                        !c.is_ascii_alphanumeric() && c != '_'
                                    })
                            })
                        }) {
                            kind = if label == "review_requested" {
                                "review_request"
                            } else {
                                "read_request"
                            };
                            if entry.relationship == "mentioned"
                                || (entry.relationship == "read_requested"
                                    && label == "review_requested")
                            {
                                entry.relationship = label.into();
                            }
                        }
                    }
                }
            }
            let evidence = Evidence {
                kind: kind.into(),
                role: role.into(),
                record,
                source: None,
            };
            if !entry.evidence.contains(&evidence) {
                entry.evidence.push(evidence);
            }
        }
        ensure!(
            found.len() <= 256,
            "Too many PR relationships in one snapshot"
        );
        Ok(())
    };
    let rows = redact::parse(compact)?;
    ensure!(
        rows.first()
            .is_some_and(|r| r["type"] == "header" && matches!(r["version"].as_u64(), Some(1 | 2))),
        "Unsupported compact transcript header"
    );
    for (record, row) in rows.iter().enumerate().skip(1) {
        match row["type"].as_str() {
            Some("message") => {
                let role = row["role"]
                    .as_str()
                    .context("PR evidence message lacks role")?;
                if let Some(text) = row["content"].as_str() {
                    add(text, role, record, false)?;
                } else {
                    add(&reference_text(&row["content"]), role, record, false)?;
                }
            }
            Some("tool.execution_start") => {
                add(
                    &reference_text(&row["data"]["arguments"]),
                    "tool_input",
                    record,
                    false,
                )?;
                if row["data"]["toolName"] == "Bash" {
                    if let Some(command) = row["data"]["arguments"]["command"].as_str() {
                        if let Some(targets) = gh_lookup(command, context)? {
                            for target in targets {
                                add(&target.url(), "tool_input", record, false)?;
                            }
                        }
                    }
                }
            }
            Some("tool.execution_complete") => add(
                &reference_text(&row["data"]["result"]),
                "tool_output",
                record,
                false,
            )?,
            _ => add(&reference_text(&row["data"]), "session", record, false)?,
        }
    }
    if let Some(links) = provider_links
        .map(|v| {
            v.as_array()
                .context("Provider PR references must be an array")
        })
        .transpose()?
    {
        for (index, link) in links.iter().enumerate() {
            if link["relationship"] == "provider_linked" {
                add(
                    link["url"].as_str().context("Provider PR link lacks URL")?,
                    "provider_link",
                    index,
                    true,
                )?;
            }
        }
    }
    Ok(Collection {
        version: 1,
        context_repository: context.map(repository).transpose()?,
        relationships: found.into_values().collect(),
    })
}

fn read(repo: &Repo, revision: &str, path: &str) -> Result<String> {
    let output = git::run(
        &repo.root,
        &["show", &format!("{revision}:{path}")],
        &BTreeMap::new(),
        None,
    )?;
    ensure!(
        output.success,
        "Cannot read checkpoint relationship data: {}",
        output.stderr
    );
    ensure!(
        output.stdout.len() <= 64 * 1024 * 1024,
        "Checkpoint relationship data exceeds size limit"
    );
    Ok(output.stdout)
}

pub fn related(repo: &Repo, target: &Target) -> Result<Value> {
    let context = origin_repository(repo)?;
    let references = repo.g(&[
        "for-each-ref",
        "--format=%(refname)",
        "refs/trailvault/checkpoints/",
    ])?;
    ensure!(
        references.lines().count() <= 10000,
        "Too many checkpoints; use a scoped repository cache"
    );
    let mut snapshots = vec![];
    for reference in references.lines() {
        let id = reference
            .rsplit('/')
            .next()
            .context("Missing checkpoint identity")?;
        ensure!(
            capture::checkpoint_ref(id)? == reference,
            "Invalid checkpoint namespace"
        );
        let commit = repo.g(&["rev-parse", reference])?;
        let root: Value = serde_json::from_str(&read(repo, &commit, "metadata.json")?)?;
        ensure!(
            root["checkpoint_id"] == id,
            "Checkpoint relationship identity mismatch"
        );
        let sessions = root["sessions"]
            .as_array()
            .context("Missing checkpoint session index")?;
        ensure!(sessions.len() <= 256, "Too many checkpoint sessions");
        for (number, index) in sessions.iter().enumerate() {
            ensure!(
                index["metadata"] == format!("/{number}/metadata.json"),
                "Invalid relationship session index"
            );
            let details: Value =
                serde_json::from_str(&read(repo, &commit, &format!("{number}/metadata.json"))?)?;
            ensure!(
                details["checkpoint_id"] == id
                    && details["session_id"].as_str().is_some_and(|s| !s.is_empty()
                        && s.len() <= 128
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))),
                "Relationship session identity mismatch"
            );
            ensure!(
                index["compact_transcript"] == format!("/{number}/transcript.jsonl"),
                "Invalid relationship transcript index"
            );
            let compact = read(repo, &commit, &format!("{number}/transcript.jsonl"))?;
            let evidence = if index.get("evidence").is_some() {
                ensure!(
                    index["evidence"] == format!("/{number}/evidence.json"),
                    "Invalid PR evidence index"
                );
                Some(serde_json::from_str::<Value>(&read(
                    repo,
                    &commit,
                    &format!("{number}/evidence.json"),
                )?)?)
            } else {
                None
            };
            let stored = details
                .get("pr_relationships")
                .map(|v| serde_json::from_value::<Collection>(v.clone()))
                .transpose()?;
            let links = collect(
                &compact,
                stored
                    .as_ref()
                    .map(|l| l.context_repository.as_deref())
                    .unwrap_or(context.as_deref()),
                evidence.as_ref().and_then(|e| e.get("pr_references")),
            )?;
            if let Some(stored) = stored {
                ensure!(
                    stored == links,
                    "Stored PR relationships do not match transcript evidence"
                );
            }
            let files: Vec<String> =
                serde_json::from_value(details.get("files_touched").cloned().unwrap_or(json!([])))
                    .context("Invalid captured file-change evidence")?;
            let activity = activities(&compact, &links, &files)?;
            if let Some(stored) = details.get("pr_activities") {
                validate_activities(stored, &activity)?;
            }
            for relationship in links.relationships.iter().filter(|r| &r.target == target) {
                let activity = activity
                    .relationships
                    .iter()
                    .find(|activity| &activity.target == target)
                    .context("Missing classified PR activity")?;
                let mut relationship = serde_json::to_value(relationship)?;
                relationship["activity_version"] = json!(1);
                relationship["activities"] = serde_json::to_value(&activity.activities)?;
                relationship["activity_evidence"] = serde_json::to_value(&activity.evidence)?;
                snapshots.push(json!({"checkpoint":id,"checkpoint_ref":reference,"checkpoint_commit":commit,
                    "session_id":details["session_id"],"agent":details["agent"],"session_directory":number,
                    "capture_coverage":details.get("capture_coverage"),"publication":details.get("publication"),
                    "relationship":relationship}));
            }
        }
    }
    Ok(
        json!({"version":1,"target":target,"snapshots":snapshots,"discovery":"local_checkpoint_refs; no implicit network fetch",
        "contribution_verification":"PR mentions and source-HEAD snapshots are not contribution proof"}),
    )
}
