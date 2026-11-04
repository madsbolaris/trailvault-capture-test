mod support;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use support::{Fixture, put};
use trailvault::{capture, git, native::Provider};

fn state_path(f: &Fixture, id: &str) -> Result<PathBuf> {
    Ok(git::Repo::discover(&f.repo)?
        .common
        .join("trailvault-sessions")
        .join(format!("{id}.json")))
}

fn state(f: &Fixture, id: &str) -> Result<Value> {
    git::read(&state_path(f, id)?)
}

fn metadata(f: &Fixture, checkpoint: &str) -> Result<Value> {
    Ok(serde_json::from_str(&f.g(&[
        "show",
        &format!("{}:0/metadata.json", capture::checkpoint_ref(checkpoint)?),
    ])?)?)
}

fn full_transcript(f: &Fixture, checkpoint: &str) -> Result<String> {
    let output = git::run(
        &f.repo,
        &[
            "show",
            &format!("{}:0/full.jsonl", capture::checkpoint_ref(checkpoint)?),
        ],
        &f.env,
        None,
    )?;
    ensure!(output.success, "Cannot read checkpoint transcript");
    Ok(output.stdout)
}

fn commit(f: &Fixture, file: &str) -> Result<String> {
    f.g(&["add", file])?;
    f.g(&["commit", "-m", "Client metadata fixture"])?;
    capture::trailer(
        &f.g(&["log", "-1", "--format=%B"])?,
        "Trailvault-Checkpoint",
    )
    .context("Missing fixture checkpoint")
}

fn hosted_fixture(provider: Provider) -> Result<(Fixture, Value)> {
    let f = Fixture::new()?;
    f.g(&[
        "remote",
        "add",
        "origin",
        "https://github.com/example/client-test.git",
    ])?;
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let path = f.temp.path().join("hosted-transcript.json");
    let raw = match provider {
        Provider::Claude => format!(
            "{}\n",
            json!({"type":"user","sessionId":id,"cwd":"/hosted/repo","message":{"content":"Hosted metadata fixture"}})
        ),
        Provider::Codex => {
            let exchange = |index: usize, method: &str, params: Value, result: Value| json!({"request":{"id":index,"method":method,"params":params},"response":{"id":index,"result":result}});
            serde_json::to_string(&json!({
                "schema":"trailvault.codex-app-server-observation.v1","threadId":id,
                "collectedAt":timestamp,"collection":"passive_provider_client_rpc",
                "responses":[
                    exchange(0, "thread/read", json!({"threadId":id,"includeTurns":false}),
                        json!({"thread":{"id":id,"cwd":"/hosted/repo","source":{"custom":"codex-cloud-agent"},
                            "status":{"type":"idle"},"environments":[{"environmentId":"ccarenv_b64_U3ludGhldGlj"}]}})),
                    exchange(1, "thread/turns/list", json!({"threadId":id,"sortDirection":"asc"}),
                        json!({"data":[{"id":"turn-1","status":"completed","items":[],"itemsView":"notLoaded"}],"nextCursor":null})),
                    exchange(2, "thread/items/list", json!({"threadId":id,"sortDirection":"asc"}),
                        json!({"data":[{"turnId":"turn-1","item":{"id":"user-1","type":"userMessage",
                            "content":[{"type":"text","text":"Hosted metadata fixture"}]}}],"nextCursor":null}))
                ]
            }))?
        }
    };
    put(&path, raw)?;
    let payload = json!({
        "native_session_id":id,"transcript_path":path,"provenance":{
            "remote_session_id":if provider == Provider::Codex {id.clone()} else {"session_SyntheticClient12345".into()},
            "repository":"example/client-test","source_commit":f.g(&["rev-parse","HEAD"])?,
            "source_branch":"test/hosted","transcript_cwd":"/hosted/repo","collected_at":timestamp,
            "collection":if provider == Provider::Codex {"codex-app-server"} else {"claude-teleport"},
            "completeness":if provider == Provider::Codex {"available-persisted-history"} else {"handoff-may-be-compacted"},
            "identity_basis":"collector_asserted"
        }
    });
    Ok((f, payload))
}

fn import(f: &Fixture, provider: Provider, payload: &Value) -> Result<String> {
    Ok(support::invoke(
        &f.repo,
        &["capture-cloud", provider.name(), "-"],
        &f.env,
        Some(payload),
    )?
    .trim()
    .into())
}

#[test]
fn copilot_local_default_explicit_replacement_and_checkpoint_metadata() -> Result<()> {
    let mut f = Fixture::new()?;
    // The collector's environment is not evidence about the captured client.
    f.env.insert("GITHUB_ACTIONS".into(), "true".into());
    f.env.insert("TERM_PROGRAM".into(), "vscode".into());
    let session = f.capture("client-change.txt")?;
    ensure!(
        state(&f, &session.id)?["client"] == json!({"app":"copilot-cli","environment":"local"})
    );
    let mut payload = session.payload.clone();
    payload["client"] = json!({"app":"vscode","environment":"cloud"});
    f.hook("session-start", &payload)?;
    f.hook("agent-stop", &session.payload)?;
    ensure!(state(&f, &session.id)?["client"] == payload["client"]);
    // Replacing with an app-only assertion must not retain an old environment.
    payload["client"] = json!({"app":"cli"});
    f.hook("session-start", &payload)?;
    f.hook("agent-stop", &session.payload)?;
    let checkpoint = commit(&f, "client-change.txt")?;
    ensure!(metadata(&f, &checkpoint)?["client"] == json!({"app":"cli"}));
    let full = full_transcript(&f, &checkpoint)?;
    ensure!(full == fs::read_to_string(session.transcript)?);
    Ok(())
}

#[test]
fn explicit_cloud_adapter_does_not_mislabel_copilot_as_local_cli() -> Result<()> {
    let f = Fixture::new()?;
    f.cli(&["enable", "--cloud", "--skip-push-sessions"])?;
    let session = f.capture("cloud-client.txt")?;
    ensure!(state(&f, &session.id)?["client"] == json!({"app":"copilot","environment":"cloud"}));
    let mut payload = session.payload.clone();
    payload["client"] = json!({"app":"copilot-cli","environment":"cloud"});
    f.hook("session-start", &payload)?;
    f.hook("agent-stop", &session.payload)?;
    ensure!(state(&f, &session.id)?["client"] == payload["client"]);
    let before = fs::read(state_path(&f, &session.id)?)?;
    payload["client"] = json!({"app":"copilot","environment":"local"});
    ensure!(f.hook("session-start", &payload).is_err());
    ensure!(fs::read(state_path(&f, &session.id)?)? == before);
    let checkpoint = commit(&f, "cloud-client.txt")?;
    ensure!(
        metadata(&f, &checkpoint)?["client"] == json!({"app":"copilot-cli","environment":"cloud"})
    );
    Ok(())
}

#[test]
fn native_clients_default_truthfully_and_preserve_explicit_metadata_in_checkpoints() -> Result<()> {
    let mut f = Fixture::new()?;
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
        let raw = format!(
            "{}\n",
            if provider == Provider::Claude {
                json!({"type":"user","sessionId":id,"cwd":f.repo,"message":{"content":"Native metadata fixture"}})
            } else {
                json!({"type":"session_meta","payload":{"id":id,"cwd":f.repo}})
            }
        );
        put(&path, &raw)?;
        let mut payload = json!({"sessionId":id,"transcriptPath":path});
        let call = |payload: &Value, boundary: &str, checkpoint: bool| {
            let mut args = vec!["capture", provider.name(), boundary, "-"];
            if checkpoint {
                args.push("--checkpoint");
            }
            support::invoke(&f.repo, &args, &f.env, Some(payload))
        };
        call(&payload, "session-start", false)?;
        ensure!(state(&f, &id)?["client"] == json!({"app":provider.name(),"environment":"local"}));
        payload["client"] = json!({"app":"vscode"});
        call(&payload, "session-start", false)?;
        payload.as_object_mut().unwrap().remove("client");
        let checkpoint = call(&payload, "agent-stop", true)?;
        ensure!(state(&f, &id)?["client"] == json!({"app":"vscode"}));
        ensure!(metadata(&f, checkpoint.trim())?["client"] == json!({"app":"vscode"}));
        let full = full_transcript(&f, checkpoint.trim())?;
        ensure!(full == raw);
    }
    Ok(())
}

#[test]
fn hosted_clients_default_override_and_do_not_borrow_local_session_metadata() -> Result<()> {
    for provider in [Provider::Claude, Provider::Codex] {
        let (f, mut payload) = hosted_fixture(provider)?;
        let id = payload["native_session_id"].as_str().unwrap();
        support::invoke(
            &f.repo,
            &["capture", provider.name(), "session-start", "-"],
            &f.env,
            Some(&json!({"sessionId":id,"client":{"app":"vscode","environment":"local"}})),
        )?;
        let before = fs::read(state_path(&f, id)?)?;
        let first = import(&f, provider, &payload)?;
        ensure!(
            metadata(&f, &first)?["client"] == json!({"app":provider.name(),"environment":"cloud"})
        );
        let reference = capture::checkpoint_ref(&first)?;
        let first_sha = f.g(&["rev-parse", &reference])?;
        payload["client"] = json!({"app":"cli"});
        let second = import(&f, provider, &payload)?;
        ensure!(metadata(&f, &second)?["client"] == payload["client"]);
        ensure!(f.g(&["rev-parse", &reference])? == first_sha);
        ensure!(
            fs::read(state_path(
                &f,
                payload["native_session_id"].as_str().unwrap()
            )?)? == before
        );
        payload["client"] = json!({"app":provider.name(),"environment":"local"});
        let refs_before = f.g(&[
            "for-each-ref",
            "--format=%(refname):%(objectname)",
            "refs/trailvault",
        ])?;
        ensure!(import(&f, provider, &payload).is_err());
        ensure!(
            f.g(&[
                "for-each-ref",
                "--format=%(refname):%(objectname)",
                "refs/trailvault"
            ])? == refs_before
        );
    }
    Ok(())
}

#[test]
fn invalid_client_payloads_fail_without_mutating_existing_state_or_refs() -> Result<()> {
    let f = Fixture::new()?;
    let id = uuid::Uuid::new_v4().to_string();
    let initial = json!({"sessionId":id,"client":{"app":"copilot"}});
    f.hook("session-start", &initial)?;
    let before = fs::read(state_path(&f, &id)?)?;
    let (hosted, hosted_base) = hosted_fixture(Provider::Claude)?;
    for invalid in [
        json!(null),
        json!([]),
        json!("vscode"),
        json!({}),
        json!({"app":"VSCode"}),
        json!({"app":"unknown"}),
        json!({"app":1}),
        json!({"app":"codex","environment":"remote"}),
        json!({"app":"codex","environment":null}),
        json!({"app":"codex","editor":"vscode"}),
    ] {
        let input = json!({"sessionId":id,"client":invalid});
        ensure!(f.hook("session-start", &input).is_err());
        let error = support::invoke(
            &f.repo,
            &["capture", "claude-code", "session-start", "-"],
            &f.env,
            Some(&input),
        );
        ensure!(error.is_err());
        ensure!(fs::read(state_path(&f, &id)?)? == before);
        let mut payload = hosted_base.clone();
        payload["client"] = invalid;
        ensure!(import(&hosted, Provider::Claude, &payload).is_err());
        ensure!(
            hosted
                .g(&["for-each-ref", "--format=%(refname)", "refs/trailvault"])?
                .is_empty()
        );
    }
    Ok(())
}

#[test]
fn client_schema_roundtrips_all_apps_and_optional_environments() -> Result<()> {
    for app in [
        "vscode",
        "copilot",
        "copilot-cli",
        "codex",
        "claude-code",
        "cli",
    ] {
        for environment in [None, Some("local"), Some("cloud")] {
            let mut value = json!({"app":app});
            if let Some(environment) = environment {
                value["environment"] = json!(environment);
            }
            let parsed: capture::Client = serde_json::from_value(value.clone())?;
            ensure!(serde_json::to_value(parsed)? == value);
        }
    }
    Ok(())
}

#[test]
fn legacy_state_and_new_checkpoints_without_client_remain_unknown() -> Result<()> {
    let f = Fixture::new()?;
    let session = f.capture("legacy-client.txt")?;
    let mut legacy = state(&f, &session.id)?;
    legacy.as_object_mut().unwrap().remove("client");
    git::save(&state_path(&f, &session.id)?, &legacy)?;
    let repo = git::Repo::discover(&f.repo)?;
    let loaded = capture::sessions(&repo)?;
    ensure!(loaded[0].client.is_none());
    ensure!(serde_json::to_value(&loaded[0])?.get("client").is_none());
    let checkpoint = commit(&f, "legacy-client.txt")?;
    ensure!(metadata(&f, &checkpoint)?.get("client").is_none());
    let reference = capture::checkpoint_ref(&checkpoint)?;
    let original = f.g(&["rev-parse", &reference])?;
    f.hook("session-start", &session.payload)?;
    ensure!(
        state(&f, &session.id)?["client"] == json!({"app":"copilot-cli","environment":"local"})
    );
    ensure!(f.g(&["rev-parse", &reference])? == original);
    ensure!(metadata(&f, &checkpoint)?.get("client").is_none());
    Ok(())
}
