use crate::{
    capture,
    git::{self, Repo},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};

const POLICY: &str = ".trailvault/policy.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Optional,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    version: u32,
    logging: Mode,
    #[serde(default)]
    destination: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Decision {
    Pending,
    Approved,
    Declined,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Consent {
    version: u32,
    policy_sha256: String,
    origin_sha256: String,
    logging: Mode,
    decision: Decision,
    recorded_at: String,
}

struct Current {
    policy: Option<Policy>,
    hash: String,
    origin: String,
    saved: Option<Consent>,
}

fn destination(policy: &Policy) -> Result<String> {
    let value = policy
        .destination
        .as_deref()
        .context("Logging policy must declare destination")?;
    if value == "origin" {
        return Ok(value.into());
    }
    let repository = value
        .strip_prefix("github:")
        .context("Policy destination must be origin or github:OWNER/REPO")?;
    let (owner, repo) = repository
        .split_once('/')
        .context("Expected github:OWNER/REPO")?;
    ensure!(
        !owner.is_empty()
            && owner
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "Invalid destination owner"
    );
    ensure!(
        !repo.is_empty()
            && ![".", ".."].contains(&repo)
            && repo
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
        "Invalid destination repository"
    );
    Ok(format!("https://github.com/{owner}/{repo}.git"))
}

fn read(repo: &Repo) -> Result<(Option<Policy>, String)> {
    let entry = repo.g(&["ls-tree", "HEAD", "--", POLICY])?;
    if entry.is_empty() {
        return Ok((None, "none".into()));
    }
    let (fields, path) = entry
        .split_once('\t')
        .context("Invalid policy tree entry")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    ensure!(
        path == POLICY && fields.len() == 3 && fields[0] == "100644" && fields[1] == "blob",
        "Logging policy must be a regular, non-executable JSON file"
    );
    let length: usize = repo.g(&["cat-file", "-s", fields[2]])?.parse()?;
    ensure!(length <= 65536, "Logging policy exceeds 64 KiB");
    let output = git::run(
        &repo.root,
        &["cat-file", "blob", fields[2]],
        &Default::default(),
        None,
    )?;
    ensure!(
        output.success,
        "Cannot read logging policy: {}",
        output.stderr
    );
    let policy: Policy = serde_json::from_str(&output.stdout).context("Invalid logging policy")?;
    ensure!(policy.version == 1, "Unsupported logging policy version");
    if policy.logging != Mode::Off {
        destination(&policy)?;
    }
    Ok((Some(policy), capture::hash(output.stdout.as_bytes())))
}

fn receipt(repo: &Repo) -> std::path::PathBuf {
    repo.git_dir.join("trailvault-consent.json")
}

fn current(repo: &Repo) -> Result<Current> {
    let (policy, hash) = read(repo)?;
    let path = receipt(repo);
    crate::install::ensure_plain(&path, false)?;
    let saved: Option<Consent> = git::optional(&path)?;
    if let Some(saved) = &saved {
        ensure!(saved.version == 1, "Unsupported consent receipt version");
    }
    let origin = if policy.is_some() || saved.is_some() {
        capture::hash(serde_json::to_vec(&json!({
            "fetch":repo.g(&["remote","get-url","--all","origin"]).context("Logging policy requires an origin remote")?,
            "push":repo.g(&["remote","get-url","--push","--all","origin"]).context("Cannot inspect origin publication destinations")?
        }))?)
    } else {
        "none".into()
    };
    Ok(Current {
        policy,
        hash,
        origin,
        saved,
    })
}

impl Current {
    fn mode(&self) -> Mode {
        self.policy
            .as_ref()
            .map(|policy| policy.logging)
            .unwrap_or(Mode::Off)
    }
    fn approved(&self) -> bool {
        self.saved.as_ref().is_some_and(|saved| {
            saved.decision == Decision::Approved
                && saved.policy_sha256 == self.hash
                && saved.origin_sha256 == self.origin
        })
    }
    fn required(&self) -> bool {
        self.mode() == Mode::Required
            || self
                .saved
                .as_ref()
                .is_some_and(|saved| saved.logging == Mode::Required)
    }
    fn save(&self, repo: &Repo, decision: Decision) -> Result<()> {
        git::save(
            &receipt(repo),
            &Consent {
                version: 1,
                policy_sha256: self.hash.clone(),
                origin_sha256: self.origin.clone(),
                logging: self.mode(),
                decision,
                recorded_at: chrono::Utc::now().to_rfc3339(),
            },
        )
    }
}

pub fn capture_allowed(repo: &Repo) -> Result<bool> {
    let current = current(repo)?;
    Ok(if current.policy.is_none() && current.saved.is_none() {
        true
    } else {
        current.mode() != Mode::Off && current.approved()
    })
}

pub fn enforce_write(repo: &Repo) -> Result<()> {
    let current = current(repo)?;
    if current.required() {
        ensure!(
            current.approved(),
            "This repository requires session logs; consent is absent, declined, or its policy/origin changed. Run `trailvault consent` to review it."
        );
        ensure!(
            publication_allowed(repo)?,
            "This repository requires publication to the approved logging destination. Run `trailvault consent`."
        );
        let config = capture::settings(repo)?;
        ensure!(
            config["enabled"] == true
                && config.pointer("/strategy_options/push_sessions") == Some(&json!(true)),
            "This repository requires logging and publication to remain enabled. Run `trailvault consent` to review it."
        );
    }
    Ok(())
}

pub fn publication_allowed(repo: &Repo) -> Result<bool> {
    let current = current(repo)?;
    if current.policy.is_none() && current.saved.is_none() {
        return Ok(true);
    }
    if !current.approved() || current.mode() == Mode::Off {
        return Ok(false);
    }
    let policy = current.policy.as_ref().context("Missing approved policy")?;
    let config = capture::settings(repo)?;
    Ok(config["checkpoint_push_remote"].as_str() == Some(destination(policy)?.as_str()))
}

pub fn require_approved(repo: &Repo) -> Result<()> {
    let current = current(repo)?;
    if current.policy.is_some() || current.saved.is_some() {
        ensure!(
            current.mode() != Mode::Off && current.approved(),
            "Repository logging needs consent before enabling capture or publication. Run `trailvault consent`."
        );
    }
    Ok(())
}

fn prepare(repo: &Repo, executable: &Path) -> Result<Current> {
    let current = current(repo)?;
    {
        let _lock = repo.lock()?;
        current.save(repo, Decision::Pending)?;
    }
    capture::enable_for_policy(repo, executable)?;
    {
        let _lock = repo.lock()?;
        capture::update_local(
            repo,
            &json!({"enabled":false,"strategy_options":{"push_sessions":false}}),
        )?;
    }
    Ok(current)
}

pub fn observe(repo: &Repo, executable: &Path) -> Result<()> {
    let current = current(repo)?;
    if current.saved.is_none() && current.mode() != Mode::Off {
        prepare(repo, executable)?;
        eprintln!(
            "trailvault: repository logging policy needs consent. Capture is paused; run `trailvault consent`."
        );
    }
    Ok(())
}

pub fn review(repo: &Repo, executable: &Path, answer: Option<bool>) -> Result<()> {
    let initial = current(repo)?;
    if initial.mode() == Mode::Off && initial.saved.is_none() {
        println!("This repository does not request session logging.");
        return Ok(());
    }
    let initial = prepare(repo, executable)?;
    if initial.mode() == Mode::Off {
        let _lock = repo.lock()?;
        initial.save(repo, Decision::Approved)?;
        println!("Reviewed policy: repository logging is off. Capture remains disabled.");
        return Ok(());
    }
    let policy = initial.policy.as_ref().context("Missing logging policy")?;
    eprintln!(
        "\nTrailvault: this repository {} session logs.",
        if policy.logging == Mode::Required {
            "requires"
        } else {
            "requests optional"
        }
    );
    if let Some(description) = &policy.description {
        eprintln!("Policy: {}", serde_json::to_string(description)?);
    }
    eprintln!(
        "Approval permits capture and automatic publication of redacted coding-agent prompts, replies, native transcripts, tool activity, PR/lifecycle evidence and task metadata to {}.",
        serde_json::to_string(policy.destination.as_ref().context("Missing destination")?)?
    );
    eprintln!(
        "Redaction is not a guarantee of removing every secret. Declining never enables capture. Required-policy refusal blocks managed commits and pushes."
    );
    eprintln!(
        "Approval can also publish previously queued local checkpoints; review existing captures before approving."
    );
    let approved = match answer {
        Some(answer) => answer,
        None => {
            ensure!(
                io::stdin().is_terminal()
                    && std::env::var_os("GH_PROMPT_DISABLED").is_none_or(|value| value.is_empty()),
                "Logging consent requires an interactive terminal. Clone retained with capture disabled. Run `trailvault consent`, or explicitly use `trailvault consent --approve` / `--decline`."
            );
            loop {
                eprint!("Approve session logging and publication? [y/N]: ");
                io::stderr().flush()?;
                let mut line = String::new();
                ensure!(
                    io::stdin().read_line(&mut line)? > 0,
                    "Consent input ended; capture remains disabled"
                );
                match line.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => break true,
                    "" | "n" | "no" => break false,
                    _ => eprintln!("Enter yes or no."),
                }
            }
        }
    };
    let latest = current(repo)?;
    ensure!(
        latest.hash == initial.hash && latest.origin == initial.origin,
        "Policy or origin changed during consent; review it again"
    );
    {
        let _lock = repo.lock()?;
        latest.save(
            repo,
            if approved {
                Decision::Approved
            } else {
                Decision::Declined
            },
        )?;
        let mut settings =
            json!({"enabled":approved,"strategy_options":{"push_sessions":approved}});
        if approved {
            settings["checkpoint_push_remote"] = json!(destination(policy)?);
        }
        capture::update_local(repo, &settings)?;
    }
    println!(
        "{}",
        if approved {
            "Logging and publication approved for this repository policy."
        } else {
            "Logging declined. No Trailvault capture or publication is enabled."
        }
    );
    if !approved && policy.logging == Mode::Required {
        eprintln!(
            "trailvault: repository retained for browsing; managed commits and pushes are blocked until you approve its policy."
        );
    }
    Ok(())
}

pub fn after_clone(repo: &Repo, executable: &Path) -> Result<()> {
    if current(repo)?.mode() != Mode::Off {
        review(repo, executable, None)?;
    }
    Ok(())
}

pub fn revoke(repo: &Repo) -> Result<()> {
    let current = current(repo)?;
    if current.policy.is_some() || current.saved.is_some() {
        let _lock = repo.lock()?;
        current.save(repo, Decision::Declined)?;
    }
    Ok(())
}

pub fn status(repo: &Repo) -> Result<Value> {
    let current = current(repo)?;
    Ok(
        json!({"policy":current.policy,"approved_for_current_policy":current.approved(),
        "requires_logs":current.required(),"consent":current.saved}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_are_declarative_not_commands_or_arbitrary_urls() {
        for value in ["origin", "github:owner/project"] {
            let policy: Policy = serde_json::from_value(
                json!({"version":1,"logging":"required","destination":value}),
            )
            .unwrap();
            assert!(destination(&policy).is_ok());
        }
        for value in [
            "",
            "github:",
            "github:owner/../escape",
            "github:owner/..",
            "github:owner/repo;echo",
            "ssh://host/repo",
            "file:///tmp/other",
            "!echo private-data",
        ] {
            let policy: Policy = serde_json::from_value(
                json!({"version":1,"logging":"required","destination":value}),
            )
            .unwrap();
            assert!(destination(&policy).is_err(), "{value}");
        }
    }

    #[test]
    fn policy_shapes_are_strict_and_do_not_coerce_types_or_unknown_fields() {
        for value in [
            json!({"version":1,"logging":"future","destination":"origin"}),
            json!({"version":"1","logging":"required","destination":"origin"}),
            json!({"version":1,"logging":"required","destination":42}),
            json!({"version":1,"logging":"required","destination":"origin","command":"echo bad"}),
            json!({"version":1,"logging":"required","destination":"origin","description":[]}),
        ] {
            assert!(serde_json::from_value::<Policy>(value).is_err());
        }
    }
}
