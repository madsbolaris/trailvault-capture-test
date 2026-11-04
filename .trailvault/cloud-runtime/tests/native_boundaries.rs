mod support;

use anyhow::{Result, ensure};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use support::{Fixture, put};
use trailvault::{
    capture,
    git::{self, Repo},
    native::{self, Provider},
    redact::Redactor,
};

#[test]
fn native_identity_exemptions_do_not_change_copilot_redaction() -> Result<()> {
    let mut redactor =
        Redactor::new(&json!({"redaction":{"custom_redactions":{"fixture":"SENSITIVE"}}}))?;
    let value = json!({"content":{"uuid":"SENSITIVE","text":"SENSITIVE"}});
    ensure!(redactor.event(&value)["content"]["uuid"] == "REDACTED");
    ensure!(redactor.native_event(&value)["content"]["uuid"] == "SENSITIVE");
    ensure!(redactor.native_event(&value)["content"]["text"] == "REDACTED");
    Ok(())
}

#[test]
fn invalid_checkpoint_flag_does_not_create_session() -> Result<()> {
    let f = Fixture::new()?;
    for provider in [Provider::Claude, Provider::Codex] {
        let payload = json!({"sessionId":uuid::Uuid::new_v4().to_string()});
        let error = support::invoke(
            &f.repo,
            &[
                "capture",
                provider.name(),
                "session-start",
                "-",
                "--checkpoint",
            ],
            &f.env,
            Some(&payload),
        )
        .unwrap_err();
        ensure!(
            format!("{error:#}").contains("Checkpoint requires a transcript snapshot boundary")
        );
    }
    ensure!(capture::sessions(&Repo::discover(&f.repo)?)?.is_empty());
    Ok(())
}

#[test]
fn stdin_and_file_payloads_have_identical_size_limits() -> Result<()> {
    let f = Fixture::new()?;
    let oversized = vec![b' '; 16 * 1024 * 1024 + 1];
    let file = f.temp.path().join("oversized.json");
    put(&file, &oversized)?;
    for source in ["-", file.to_str().unwrap()] {
        let output = git::command(
            &f.repo,
            &support::executable()?,
            &["capture", "claude-code", "session-start", source],
            &f.env,
            (source == "-").then_some(oversized.as_slice()),
            Duration::from_secs(30),
        )?;
        ensure!(!output.success && output.stderr.contains("payload exceeds 16 MiB"));
    }
    ensure!(capture::sessions(&Repo::discover(&f.repo)?)?.is_empty());
    Ok(())
}

#[test]
fn native_line_provenance_preserves_blank_lines() -> Result<()> {
    let f = Fixture::new()?;
    let id = uuid::Uuid::new_v4().to_string();
    for provider in [Provider::Claude, Provider::Codex] {
        let (first, last) = if provider == Provider::Claude {
            (
                json!({"type":"user","sessionId":id,"cwd":f.repo,"message":{"content":"baseline"}}),
                json!({"type":"assistant","sessionId":id,"cwd":f.repo,"message":{"content":"https://github.com/example/test/pull/1"}}),
            )
        } else {
            (
                json!({"type":"session_meta","payload":{"id":id,"cwd":f.repo}}),
                json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"https://github.com/example/test/pull/1"}]}}),
            )
        };
        let raw = format!("\n{first}\n\n{last}\n");
        let parsed = native::parse(provider, &id, &f.repo, &raw)?;
        ensure!(parsed.references.len() == 1 && parsed.references[0]["native_line"] == 4);
        ensure!(
            parsed
                .observations
                .iter()
                .filter(|o| o["kind"] == "native.event")
                .map(|o| o["native_line"].as_u64().unwrap())
                .collect::<Vec<_>>()
                == vec![2, 4]
        );
    }
    Ok(())
}

#[test]
fn oversized_and_symlinked_native_sources_are_rejected_without_state_loss() -> Result<()> {
    let mut f = Fixture::new()?;
    let repo = Repo::discover(&f.repo)?;
    for provider in [Provider::Claude, Provider::Codex] {
        let id = uuid::Uuid::new_v4().to_string();
        let home = f.temp.path().join(provider.name());
        f.env.insert(
            if provider == Provider::Claude {
                "CLAUDE_CONFIG_DIR"
            } else {
                "CODEX_HOME"
            }
            .into(),
            support::text(&home),
        );
        let path = home
            .join(if provider == Provider::Claude {
                "projects"
            } else {
                "sessions"
            })
            .join(if provider == Provider::Claude {
                format!("{id}.jsonl")
            } else {
                format!("rollout-{id}.jsonl")
            });
        support::invoke(
            &f.repo,
            &["capture", provider.name(), "session-start", "-"],
            &f.env,
            Some(&json!({"sessionId":id})),
        )?;
        let state_path = repo
            .common
            .join("trailvault-sessions")
            .join(format!("{id}.json"));
        let before = fs::read(&state_path)?;
        put(&path, "")?;
        fs::File::options()
            .write(true)
            .open(&path)?
            .set_len(64 * 1024 * 1024 + 1)?;
        let payload = json!({"sessionId":id,"transcriptPath":path});
        let error = support::invoke(
            &f.repo,
            &["capture", provider.name(), "post-tool-use", "-"],
            &f.env,
            Some(&payload),
        )
        .unwrap_err();
        ensure!(format!("{error:#}").contains("Native transcript exceeds 64 MiB"));
        ensure!(fs::read(&state_path)? == before);
        fs::remove_file(&path)?;
        let outside = f.temp.path().join(format!("external-{id}.jsonl"));
        put(&outside, "{}\n")?;
        std::os::unix::fs::symlink(&outside, &path)?;
        let error = support::invoke(
            &f.repo,
            &["capture", provider.name(), "post-tool-use", "-"],
            &f.env,
            Some(&payload),
        )
        .unwrap_err();
        ensure!(format!("{error:#}").contains("Native transcript must remain inside"));
        ensure!(fs::read(&state_path)? == before);
    }
    let result = git::command(
        &f.repo,
        Path::new("git"),
        &["status", "--porcelain"],
        &BTreeMap::new(),
        None,
        Duration::from_secs(30),
    )?;
    ensure!(result.success);
    Ok(())
}
