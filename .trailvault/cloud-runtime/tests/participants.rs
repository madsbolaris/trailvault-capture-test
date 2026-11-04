mod support;

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use support::Fixture;
use trailvault::{capture, git, native::Provider};

#[test]
fn copilot_participants_survive_later_boundaries_and_checkpoint_publication() -> Result<()> {
    let f = Fixture::new()?;
    let session = f.capture("participant-change.txt")?;
    let mut payload = session.payload.clone();
    payload["participants"] = json!([{"github_login":"fixture-human"}]);
    f.hook("session-start", &payload)?;
    f.hook("agent-stop", &session.payload)?;
    f.g(&["add", "participant-change.txt"])?;
    f.g(&["commit", "-m", "Capture explicit participant"])?;
    let id = capture::trailer(
        &f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .unwrap();
    let reference = capture::checkpoint_ref(&id)?;
    let metadata: Value =
        serde_json::from_str(&f.g(&["show", &format!("{reference}:0/metadata.json")])?)?;
    ensure!(metadata["participants"] == payload["participants"]);
    ensure!(metadata["participant_identity_basis"] == "capture_input");
    let full = f.g(&["show", &format!("{reference}:0/full.jsonl")])?;
    ensure!(
        !full.contains("fixture-human"),
        "Native transcript must not be rewritten"
    );
    Ok(())
}

#[test]
fn native_participants_are_explicit_optional_and_clearable() -> Result<()> {
    let f = Fixture::new()?;
    let repo = git::Repo::discover(&f.repo)?;
    for provider in [Provider::Claude, Provider::Codex] {
        let id = uuid::Uuid::new_v4().to_string();
        let mut payload = json!({"sessionId":id});
        capture::native_hook(&repo, provider, "session-start", &payload)?;
        let state = || -> Result<Value> {
            Ok(serde_json::to_value(
                capture::sessions(&repo)?
                    .into_iter()
                    .find(|state| state.session_id == id)
                    .unwrap(),
            )?)
        };
        ensure!(state()?.get("participants").is_none());
        payload["participants"] =
            json!([{"github_login":"fixture-human"},{"github_login":"second-human"}]);
        capture::native_hook(&repo, provider, "session-start", &payload)?;
        ensure!(state()?["participants"] == payload["participants"]);
        capture::native_hook(&repo, provider, "session-start", &json!({"sessionId":id}))?;
        ensure!(state()?["participants"] == payload["participants"]);
        payload["participants"] = json!([]);
        capture::native_hook(&repo, provider, "session-start", &payload)?;
        ensure!(state()?.get("participants").is_none());
    }
    Ok(())
}

#[test]
fn invalid_participants_fail_without_changing_session_state() -> Result<()> {
    let f = Fixture::new()?;
    let repo = git::Repo::discover(&f.repo)?;
    for participants in [
        json!(null),
        json!({}),
        json!([{"github_login":""}]),
        json!([{"github_login":"-bad"}]),
        json!([{"github_login":"bad-"}]),
        json!([{"github_login":"bad--login"}]),
        json!([{"github_login":"bad/name"}]),
        json!([{"github_login":"a".repeat(40)}]),
        json!([{"github_login":"user","avatar_url":"https://untrusted.invalid/avatar"}]),
        json!([{"github_login":"user"},{"github_login":"USER"}]),
        json!(
            (0..33)
                .map(|i| json!({"github_login":format!("user-{i}")}))
                .collect::<Vec<_>>()
        ),
    ] {
        let payload =
            json!({"sessionId":uuid::Uuid::new_v4().to_string(),"participants":participants});
        ensure!(capture::agent_hook(&repo, "session-start", &payload).is_err());
        ensure!(capture::native_hook(&repo, Provider::Claude, "session-start", &payload).is_err());
        ensure!(capture::sessions(&repo)?.is_empty());
    }
    Ok(())
}
