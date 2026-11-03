use crate::{
    capture,
    git::{self, Repo},
    policy, redact,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::Path,
    time::Duration,
};
use tempfile::TempDir;

const MAX_BYTES: u64 = 50 * 1024 * 1024;
const MAX_CHECKPOINTS: usize = 64;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub version: u8,
    pub repository: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub source_branch: String,
    pub checkpoints: Vec<Checkpoint>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub id: String,
    pub source_commit: String,
    pub files: BTreeMap<String, String>,
}

fn read_artifact(path: &Path) -> Result<Artifact> {
    ensure!(
        fs::metadata(path)?.len() <= MAX_BYTES,
        "Cloud artifact exceeds size limit"
    );
    serde_json::from_slice(&fs::read(path)?).context("Invalid cloud artifact")
}

pub fn export(
    repo: &Repo,
    repository: &str,
    run_id: u64,
    run_attempt: u64,
    output: &Path,
) -> Result<()> {
    ensure!(
        run_id > 0 && run_attempt > 0,
        "Invalid Actions run identity"
    );
    ensure!(
        policy::publication_allowed(repo)?,
        "Cloud export requires publication consent"
    );
    let config = capture::settings(repo)?;
    ensure!(
        config["enabled"] != false
            && config.pointer("/strategy_options/push_sessions") != Some(&json!(false)),
        "Cloud export is disabled by repository publication settings"
    );
    capture::post_commit(repo)?;
    let _lock = repo.lock()?;
    let queued: Vec<String> =
        git::optional(&repo.common.join("trailvault/push-queue.json"))?.unwrap_or_default();
    ensure!(
        queued.len() <= MAX_CHECKPOINTS,
        "Too many queued cloud checkpoints"
    );
    let mut checkpoints = Vec::new();
    for reference in queued {
        let id = reference
            .rsplit('/')
            .next()
            .context("Missing checkpoint ID")?;
        ensure!(
            capture::checkpoint_ref(id)? == reference,
            "Invalid queued checkpoint ref"
        );
        let candidates = repo.g(&[
            "log",
            "--format=%H",
            "--fixed-strings",
            &format!("--grep=Trailvault-Checkpoint: {id}"),
            "-n",
            "2",
            "HEAD",
        ])?;
        let sources: Vec<_> = candidates.lines().collect();
        ensure!(
            sources.len() == 1,
            "Checkpoint must have exactly one source commit"
        );
        let source_commit = sources[0].to_owned();
        ensure!(
            capture::trailer(
                &repo.g(&["show", "-s", "--format=%B", &source_commit])?,
                "Trailvault-Checkpoint"
            )
            .as_deref()
                == Some(id),
            "Source checkpoint trailer mismatch"
        );
        let paths = repo.g(&["ls-tree", "-r", "--name-only", "-z", &reference])?;
        let mut files = BTreeMap::new();
        for path in paths.split('\0').filter(|p| !p.is_empty()) {
            ensure!(allowed_path(path), "Unexpected checkpoint file {path}");
            let raw = git::run(
                &repo.root,
                &["show", &format!("{reference}:{path}")],
                &BTreeMap::new(),
                None,
            )?;
            files.insert(path.to_owned(), git_raw(raw)?);
        }
        checkpoints.push(Checkpoint {
            id: id.into(),
            source_commit,
            files,
        });
    }
    let artifact = Artifact {
        version: 1,
        repository: repository.into(),
        run_id,
        run_attempt,
        source_branch: repo.g(&["symbolic-ref", "--short", "HEAD"])?,
        checkpoints,
    };
    let encoded = serde_json::to_vec(&artifact)?;
    ensure!(
        encoded.len() as u64 <= MAX_BYTES,
        "Cloud artifact exceeds size limit"
    );
    git::atomic(output, &encoded)?;
    println!(
        "Exported {} redacted checkpoint(s) for run {run_id}/{run_attempt}",
        artifact.checkpoints.len()
    );
    Ok(())
}

fn git_raw(output: git::Output) -> Result<String> {
    ensure!(
        output.success,
        "Cannot read checkpoint artifact: {}",
        output.stderr
    );
    Ok(output.stdout)
}

fn allowed_path(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["metadata.json"] => true,
        [index, file] => {
            !index.is_empty()
                && index.bytes().all(|c| c.is_ascii_digit())
                && matches!(
                    *file,
                    "metadata.json"
                        | "full.jsonl"
                        | "transcript.jsonl"
                        | "prompt.txt"
                        | "content_hash.txt"
                )
        }
        ["tasks", id, "task.json"] => {
            !id.is_empty()
                && id.len() <= 128
                && !id.starts_with('.')
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        }
        _ => false,
    }
}

fn file<'a>(checkpoint: &'a Checkpoint, path: &str) -> Result<&'a str> {
    checkpoint
        .files
        .get(path)
        .map(String::as_str)
        .with_context(|| format!("Missing checkpoint file {path}"))
}

pub fn validate(checkpoint: &Checkpoint) -> Result<()> {
    capture::checkpoint_ref(&checkpoint.id)?;
    ensure!(
        matches!(checkpoint.source_commit.len(), 40 | 64)
            && checkpoint
                .source_commit
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "Invalid source commit"
    );
    ensure!(
        !checkpoint.files.is_empty() && checkpoint.files.len() <= 1024,
        "Invalid checkpoint file count"
    );
    ensure!(
        checkpoint.files.iter().all(|(p, _)| allowed_path(p)),
        "Unexpected checkpoint path"
    );
    ensure!(
        checkpoint.files.values().map(String::len).sum::<usize>() as u64 <= MAX_BYTES,
        "Checkpoint exceeds size limit"
    );
    let root: Value = serde_json::from_str(file(checkpoint, "metadata.json")?)?;
    ensure!(
        root["checkpoint_id"] == checkpoint.id,
        "Checkpoint metadata ID mismatch"
    );
    let sessions = root["sessions"]
        .as_array()
        .context("Missing session index")?;
    ensure!(
        !sessions.is_empty() && sessions.len() <= 16,
        "Invalid session count"
    );
    let mut expected = BTreeSet::from(["metadata.json".to_owned()]);
    let mut identities = BTreeSet::new();
    for (number, session) in sessions.iter().enumerate() {
        for (key, name) in [
            ("metadata", "metadata.json"),
            ("transcript", "full.jsonl"),
            ("compact_transcript", "transcript.jsonl"),
            ("prompt", "prompt.txt"),
            ("content_hash", "content_hash.txt"),
        ] {
            let path = format!("{number}/{name}");
            ensure!(
                session[key] == format!("/{path}"),
                "Invalid session artifact index"
            );
            file(checkpoint, &path)?;
            expected.insert(path);
        }
        let details: Value =
            serde_json::from_str(file(checkpoint, &format!("{number}/metadata.json"))?)?;
        let id = details["session_id"]
            .as_str()
            .context("Missing session ID")?;
        uuid::Uuid::parse_str(id).context("Invalid native session ID")?;
        ensure!(identities.insert(id.to_owned()), "Duplicate native session");
        ensure!(
            details["checkpoint_id"] == checkpoint.id,
            "Session checkpoint ID mismatch"
        );
        let full = file(checkpoint, &format!("{number}/full.jsonl"))?;
        ensure!(
            file(checkpoint, &format!("{number}/content_hash.txt"))?
                == format!("sha256:{}\n", capture::hash(full)),
            "Native content hash mismatch"
        );
        let events = redact::parse(full)?;
        ensure!(
            events
                .iter()
                .any(|e| e["type"] == "session.start" && e["data"]["sessionId"] == id),
            "Native session identity mismatch"
        );
        ensure!(
            events.iter().any(|e| e["type"] == "user.message")
                && events.iter().any(|e| e["type"] == "assistant.message"),
            "Missing native conversation"
        );
        ensure!(
            file(checkpoint, &format!("{number}/transcript.jsonl"))? == redact::compact(&events)?,
            "Compact/native transcript mismatch"
        );
    }
    ensure!(
        checkpoint
            .files
            .keys()
            .all(|p| expected.contains(p) || p.starts_with("tasks/")),
        "Unindexed session files"
    );
    for (path, content) in &checkpoint.files {
        if path.starts_with("tasks/") {
            let _: Value = serde_json::from_str(content).context("Invalid task metadata")?;
        }
    }
    Ok(())
}

pub fn publish(
    repo: &Repo,
    path: &Path,
    repository: &str,
    run_id: u64,
    run_attempt: u64,
    branch: &str,
) -> Result<()> {
    let artifact = read_artifact(path)?;
    ensure!(
        artifact.version == 1
            && artifact.repository == repository
            && artifact.run_id == run_id
            && artifact.run_attempt == run_attempt,
        "Artifact does not belong to this repository/workflow run"
    );
    ensure!(
        artifact.source_branch == branch,
        "Artifact source branch mismatch"
    );
    ensure!(
        artifact.checkpoints.len() <= MAX_CHECKPOINTS,
        "Too many cloud checkpoints"
    );
    repo.g(&["check-ref-format", &format!("refs/heads/{branch}")])?;
    repo.g(&[
        "fetch",
        "--no-tags",
        "origin",
        &format!("refs/heads/{branch}"),
    ])?;
    let source_head = repo.g(&["rev-parse", "FETCH_HEAD"])?;
    let mut ids = BTreeSet::new();
    // Validate the entire untrusted payload before creating or publishing any ref.
    for checkpoint in &artifact.checkpoints {
        validate(checkpoint)?;
        ensure!(ids.insert(&checkpoint.id), "Duplicate checkpoint ID");
        ensure!(
            git::run(
                &repo.root,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &checkpoint.source_commit,
                    &source_head
                ],
                &BTreeMap::new(),
                None
            )?
            .success,
            "Checkpoint source is outside the completed agent branch"
        );
        ensure!(
            capture::trailer(
                &repo.g(&["show", "-s", "--format=%B", &checkpoint.source_commit])?,
                "Trailvault-Checkpoint"
            )
            .as_deref()
                == Some(checkpoint.id.as_str()),
            "Source commit does not link this checkpoint"
        );
        let changed = repo.g(&[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-z",
            "-r",
            &checkpoint.source_commit,
        ])?;
        let changed: BTreeSet<_> = changed.split('\0').filter(|p| !p.is_empty()).collect();
        let root: Value = serde_json::from_str(file(checkpoint, "metadata.json")?)?;
        let touched = root["files_touched"]
            .as_array()
            .context("Missing source attribution")?;
        ensure!(
            !touched.is_empty()
                && touched
                    .iter()
                    .all(|p| p.as_str().is_some_and(|p| changed.contains(p))),
            "Checkpoint file attribution does not match the source commit"
        );
    }
    for checkpoint in &artifact.checkpoints {
        let temp = TempDir::new()?;
        let index = temp.path().join("index");
        let env = BTreeMap::from([(
            "GIT_INDEX_FILE".into(),
            index.to_str().context("Invalid index path")?.into(),
        )]);
        repo.env(&["read-tree", "--empty"], &env)?;
        for (path, content) in &checkpoint.files {
            let blob = repo.input(&["hash-object", "-w", "--stdin"], content)?;
            repo.env(
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{blob},{path}"),
                ],
                &env,
            )?;
        }
        let tree = repo.env(&["write-tree"], &env)?;
        let reference = capture::checkpoint_ref(&checkpoint.id)?;
        let remote = repo.g(&["ls-remote", "--refs", "origin", &reference])?;
        if !remote.is_empty() {
            repo.g(&["fetch", "--no-tags", "origin", &reference])?;
            ensure!(
                repo.g(&["rev-parse", "FETCH_HEAD^{tree}"])? == tree,
                "Ref collision: refusing to replace an existing checkpoint"
            );
            println!("Already published {}", checkpoint.id);
            continue;
        }
        let identity = BTreeMap::from([
            ("GIT_AUTHOR_NAME".into(), "Trailvault Publisher".into()),
            (
                "GIT_AUTHOR_EMAIL".into(),
                "trailvault@users.noreply.github.com".into(),
            ),
            ("GIT_COMMITTER_NAME".into(), "Trailvault Publisher".into()),
            (
                "GIT_COMMITTER_EMAIL".into(),
                "trailvault@users.noreply.github.com".into(),
            ),
        ]);
        let commit = git::checked(git::run(
            &repo.root,
            &["commit-tree", &tree],
            &identity,
            Some(format!("Trailvault checkpoint {}\n", checkpoint.id).as_bytes()),
        )?)?;
        // Only fixed-namespace refs are written; artifact paths never become executable files.
        repo.g(&[
            "-c",
            "core.hooksPath=/dev/null",
            "push",
            "--",
            "origin",
            &format!("{commit}:{reference}"),
        ])?;
        println!(
            "Published {} for source {}",
            checkpoint.id, checkpoint.source_commit
        );
    }
    Ok(())
}

fn backend_ids(token: &str) -> Result<(String, String)> {
    let payload = token
        .split('.')
        .nth(1)
        .context("Invalid Actions artifact credential")?;
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload)
            .context("Invalid Actions credential encoding")?,
    )?;
    for scope in claims["scp"]
        .as_str()
        .context("Missing artifact scope")?
        .split_whitespace()
    {
        let parts: Vec<_> = scope.split(':').collect();
        if parts.first() == Some(&"Actions.Results") {
            ensure!(parts.len() == 3, "Invalid Actions artifact scope");
            uuid::Uuid::parse_str(parts[1])?;
            uuid::Uuid::parse_str(parts[2])?;
            return Ok((parts[1].into(), parts[2].into()));
        }
    }
    anyhow::bail!("Actions artifact scope is unavailable")
}

fn http(config: &str, phase: &str) -> Result<Vec<u8>> {
    let response = tempfile::NamedTempFile::new()?;
    let output = git::command(
        &env::current_dir()?,
        Path::new("curl"),
        &[
            "--silent",
            "--show-error",
            "--config",
            "-",
            "--output",
            response.path().to_str().context("Invalid response path")?,
            "--write-out",
            "%{http_code}",
            "--max-time",
            "120",
        ],
        &BTreeMap::new(),
        Some(config.as_bytes()),
        Duration::from_secs(130),
    )?;
    // Never log a bearer token, signed upload URL, or response containing one.
    ensure!(
        output.success && output.stdout.starts_with('2'),
        "Actions artifact {phase} failed (HTTP {}); credential-bearing details withheld",
        output.stdout
    );
    let mut bytes = Vec::new();
    response
        .reopen()?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Artifact service response exceeds limit"
    );
    Ok(bytes)
}

fn curl_string(value: &str) -> Result<String> {
    ensure!(
        !value.contains(['\r', '\n']),
        "Invalid HTTP configuration value"
    );
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

pub fn dispatch(path: &Path) -> Result<()> {
    let artifact = read_artifact(path)?;
    ensure!(
        artifact.repository == env::var("GITHUB_REPOSITORY")?
            && artifact.run_id.to_string() == env::var("GITHUB_RUN_ID")?
            && artifact.run_attempt.to_string() == env::var("GITHUB_RUN_ATTEMPT")?,
        "Dispatch artifact does not belong to this Actions run"
    );
    let token = env::var("TRAILVAULT_DISPATCH_TOKEN")
        .context("TRAILVAULT_DISPATCH_TOKEN is required for automatic publisher dispatch")?;
    ensure!(!token.is_empty(), "Publisher dispatch token is empty");
    let body = dispatch_body(&artifact)?;
    let request = tempfile::NamedTempFile::new()?;
    git::atomic(request.path(), &serde_json::to_vec(&body)?)?;
    let config = format!(
        "url = {}\nrequest = \"POST\"\nheader = {}\nheader = \"Accept: application/vnd.github+json\"\nheader = \"Content-Type: application/json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\ndata-binary = {}\n",
        curl_string(&format!(
            "https://api.github.com/repos/{}/actions/workflows/trailvault-cloud-publish.yml/dispatches",
            artifact.repository
        ))?,
        curl_string(&format!("Authorization: Bearer {token}"))?,
        curl_string(&format!("@{}", request.path().display()))?
    );
    http(&config, "publisher dispatch")?;
    println!(
        "Dispatched trusted publisher for run {}/{}",
        artifact.run_id, artifact.run_attempt
    );
    Ok(())
}

fn dispatch_body(artifact: &Artifact) -> Result<Value> {
    let parts: Vec<_> = artifact.repository.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))),
        "Invalid dispatch repository"
    );
    ensure!(
        artifact.version == 1 && artifact.run_id > 0 && artifact.run_attempt > 0,
        "Invalid dispatch run identity"
    );
    Ok(json!({"ref":"main","inputs":{
        "source_run":artifact.run_id.to_string(),
        "source_attempt":artifact.run_attempt.to_string()
    }}))
}

pub fn upload(path: &Path) -> Result<()> {
    let artifact = read_artifact(path)?;
    let token = env::var("ACTIONS_RUNTIME_TOKEN")
        .context("Actions artifact runtime credential is unavailable")?;
    let base =
        env::var("ACTIONS_RESULTS_URL").context("Actions artifact service URL is unavailable")?;
    ensure!(
        base.starts_with("https://"),
        "Artifact service requires HTTPS"
    );
    let (run_backend, job_backend) = backend_ids(&token)?;
    let name = format!(
        "trailvault-checkpoints-{}-{}",
        artifact.run_id, artifact.run_attempt
    );
    let temp = TempDir::new()?;
    let manifest = temp.path().join("checkpoints.json");
    fs::copy(path, &manifest)?;
    let archive = temp.path().join("checkpoint.zip");
    git::checked(git::command(
        temp.path(),
        Path::new("zip"),
        &["-q", "-j", "checkpoint.zip", "checkpoints.json"],
        &BTreeMap::new(),
        None,
        Duration::from_secs(60),
    )?)?;
    let api = |method: &str, body: Value| -> Result<Value> {
        let request = temp.path().join("request.json");
        git::atomic(&request, &serde_json::to_vec(&body)?)?;
        let config = format!(
            "url = {}\nrequest = \"POST\"\nheader = {}\nheader = \"Content-Type: application/json\"\ndata-binary = {}\n",
            curl_string(&format!(
                "{}/twirp/github.actions.results.api.v1.ArtifactService/{method}",
                base.trim_end_matches('/')
            ))?,
            curl_string(&format!("Authorization: Bearer {token}"))?,
            curl_string(&format!("@{}", request.display()))?
        );
        serde_json::from_slice(&http(&config, method)?).context("Invalid artifact service response")
    };
    let created = api(
        "CreateArtifact",
        json!({
            "workflowRunBackendId":run_backend,"workflowJobRunBackendId":job_backend,
            "name":name,"version":4
        }),
    )?;
    ensure!(created["ok"] == true, "Artifact service refused creation");
    let signed = created
        .get("signedUploadUrl")
        .or_else(|| created.get("signed_upload_url"))
        .and_then(Value::as_str)
        .context("Missing artifact upload URL")?;
    ensure!(
        signed.starts_with("https://"),
        "Artifact upload requires HTTPS"
    );
    let config = format!(
        "url = {}\nrequest = \"PUT\"\nheader = \"x-ms-blob-type: BlockBlob\"\nheader = \"Content-Type: application/zip\"\ndata-binary = {}\n",
        curl_string(signed)?,
        curl_string(&format!("@{}", archive.display()))?
    );
    http(&config, "blob upload")?;
    let bytes = fs::read(&archive)?;
    let finalized = api(
        "FinalizeArtifact",
        json!({
            "workflowRunBackendId":run_backend,"workflowJobRunBackendId":job_backend,
            "name":name,"size":bytes.len().to_string(),"hash":format!("sha256:{}",capture::hash(&bytes))
        }),
    )?;
    ensure!(
        finalized["ok"] == true,
        "Artifact service refused finalization"
    );
    println!("Uploaded automatic checkpoint handoff artifact {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_pins_trusted_main_and_sends_only_source_run_identity() -> Result<()> {
        let mut artifact = Artifact {
            version: 1,
            repository: "owner/repo".into(),
            run_id: 123,
            run_attempt: 2,
            source_branch: "copilot/untrusted".into(),
            checkpoints: vec![],
        };
        assert_eq!(
            dispatch_body(&artifact)?,
            json!({"ref":"main","inputs":{
            "source_run":"123","source_attempt":"2"}})
        );
        for repository in [
            "",
            "owner/repo/extra",
            "../repo",
            "owner/repo?x=y",
            "owner/repo\nheader",
        ] {
            artifact.repository = repository.into();
            assert!(dispatch_body(&artifact).is_err());
        }
        artifact.repository = "owner/repo".into();
        artifact.run_id = 0;
        assert!(dispatch_body(&artifact).is_err());
        Ok(())
    }

    fn credential(claims: Value) -> Result<String> {
        Ok(format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
        ))
    }

    #[test]
    fn artifact_backend_ids_require_the_actions_results_scope() -> Result<()> {
        let run = "11111111-1111-4111-8111-111111111111";
        let job = "22222222-2222-4222-8222-222222222222";
        let token =
            credential(json!({"scp":format!("unrelated Actions.Results:{run}:{job} other")}))?;
        assert_eq!(backend_ids(&token)?, (run.into(), job.into()));
        for claims in [
            json!({}),
            json!({"scp":""}),
            json!({"scp":"repo:write"}),
            json!({"scp":format!("Actions.Results:{run}")}),
            json!({"scp":format!("Actions.Results:{run}:{job}:extra")}),
            json!({"scp":format!("Actions.Results:not-a-uuid:{job}")}),
            json!({"scp":format!("Actions.Results:{run}:not-a-uuid")}),
        ] {
            assert!(backend_ids(&credential(claims)?).is_err());
        }
        for token in ["", "not-a-token", "header.invalid!.signature"] {
            assert!(backend_ids(token).is_err());
        }
        Ok(())
    }

    #[test]
    fn curl_configuration_escapes_values_and_rejects_line_injection() -> Result<()> {
        assert_eq!(curl_string("quote\"slash\\")?, "\"quote\\\"slash\\\\\"");
        for value in [
            "https://example.test/\nheader=unsafe",
            "token\rheader=unsafe",
        ] {
            assert!(curl_string(value).is_err());
        }
        Ok(())
    }
}
