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
            Some("tool.execution_start") => add(
                &reference_text(&row["data"]["arguments"]),
                "tool_input",
                record,
                false,
            )?,
            Some("tool.execution_complete") => add(
                &reference_text(&row["data"]["result"]),
                "tool_output",
                record,
                false,
            )?,
            _ => {}
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
            for relationship in links.relationships.iter().filter(|r| &r.target == target) {
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
