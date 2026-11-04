use crate::{git::Repo, native};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub remote_session_id: String,
    pub repository: String,
    pub source_commit: String,
    pub source_branch: String,
    pub transcript_cwd: String,
    pub collected_at: String,
    pub collection: String,
    pub completeness: String,
    pub identity_basis: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    native_session_id: String,
    transcript_path: PathBuf,
    provenance: Provenance,
    #[serde(default)]
    participants: Vec<crate::capture::Participant>,
    #[serde(default, deserialize_with = "crate::capture::deserialize_client")]
    client: Option<crate::capture::Client>,
}

impl Provenance {
    pub fn provider(&self) -> native::Provider {
        if self.collection == crate::codex_api::COLLECTION {
            native::Provider::Codex
        } else {
            native::Provider::Claude
        }
    }
    pub fn label(&self) -> &'static str {
        if self.provider() == native::Provider::Codex {
            "Codex Cloud"
        } else {
            "Claude Code Cloud"
        }
    }
    pub fn parse(&self, id: &str, raw: &str) -> Result<native::Parsed> {
        let cwd = Path::new(&self.transcript_cwd);
        if self.provider() == native::Provider::Codex {
            Ok(crate::codex_api::parse(id, cwd, raw)?.0)
        } else {
            native::parse_hosted(id, cwd, raw)
        }
    }
}

pub fn validate(repo: &Repo, input: &Value) -> Result<(Provenance, String, String)> {
    let input: Input =
        serde_json::from_value(input.clone()).context("Invalid hosted capture payload")?;
    crate::capture::validate_participants(&input.participants)?;
    if let Some(client) = &input.client {
        client.validate_hosted()?;
    }
    let p = &input.provenance;
    uuid::Uuid::parse_str(&input.native_session_id)
        .context("Hosted native session identity must be a UUID")?;
    ensure!(
        matches!(
            p.collection.as_str(),
            "claude-teleport" | crate::codex_api::COLLECTION
        ),
        "Unsupported hosted collection method"
    );
    if p.provider() == native::Provider::Codex {
        uuid::Uuid::parse_str(&p.remote_session_id)
            .context("Invalid Codex remote session identity")?;
        ensure!(
            p.remote_session_id == input.native_session_id,
            "Codex remote/native thread identity mismatch"
        );
        ensure!(
            p.completeness == crate::codex_api::COMPLETENESS,
            "Codex API must disclose available-history limits"
        );
    } else {
        ensure!(
            p.remote_session_id
                .strip_prefix("session_")
                .is_some_and(|id| id.len() >= 8
                    && id.len() <= 128
                    && id.bytes().all(|c| c.is_ascii_alphanumeric())),
            "Invalid Claude remote session identity"
        );
        ensure!(
            p.collection == "claude-teleport",
            "Unsupported hosted collection method"
        );
        ensure!(
            p.completeness == "handoff-may-be-compacted",
            "Handoff must disclose incomplete-history risk"
        );
    }
    ensure!(
        p.identity_basis == "collector_asserted",
        "Handoff remote/native identity mapping is collector asserted, not cryptographic proof"
    );
    ensure!(
        p.source_commit.len() == 40 && p.source_commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid hosted source commit"
    );
    ensure!(
        repo.g(&[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", p.source_commit)
        ])? == p.source_commit,
        "Hosted source commit is not available in this repository"
    );
    repo.g(&[
        "check-ref-format",
        &format!("refs/heads/{}", p.source_branch),
    ])?;
    let parts: Vec<_> = p.repository.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))),
        "Invalid hosted repository identity"
    );
    let origin = repo.g(&["remote", "get-url", "origin"])?;
    ensure!(
        [
            format!("https://github.com/{}.git", p.repository),
            format!("https://github.com/{}", p.repository),
            format!("git@github.com:{}.git", p.repository)
        ]
        .contains(&origin),
        "Hosted repository does not match origin"
    );
    let cwd = Path::new(&p.transcript_cwd);
    ensure!(
        cwd.is_absolute() && !cwd.components().any(|c| matches!(c, Component::ParentDir)),
        "Hosted transcript cwd must be absolute without parent traversal"
    );
    let collected = chrono::DateTime::parse_from_rfc3339(&p.collected_at)
        .context("Invalid hosted collection timestamp")?;
    ensure!(
        collected <= chrono::Utc::now() + chrono::Duration::minutes(5),
        "Hosted collection timestamp is in the future"
    );
    ensure!(
        input.transcript_path.is_absolute(),
        "Hosted transcript path must be absolute"
    );
    let mut bytes = Vec::new();
    fs::File::open(&input.transcript_path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024 * 1024,
        "Hosted transcript exceeds 64 MiB"
    );
    let mut raw = String::from_utf8(bytes).context("Hosted transcript is not UTF-8")?;
    if p.provider() == native::Provider::Codex {
        raw = crate::codex_api::normalize(&raw, &input.native_session_id, &p.collected_at)?;
    }
    p.parse(&input.native_session_id, &raw)?;
    Ok((input.provenance, input.native_session_id, raw))
}
