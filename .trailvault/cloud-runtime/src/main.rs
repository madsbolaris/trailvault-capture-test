use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    env,
    io::{self, Read},
    path::Path,
};
use trailvault::{VERSION, capture, cloud, gh_wrapper, git::Repo, install, policy, restore};

fn main() {
    if let Err(error) = run() {
        eprintln!("trailvault: {error:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let raw: Vec<_> = env::args_os().skip(1).collect();
    if raw.first().is_some_and(|arg| arg == "gh-wrapper") {
        return gh_wrapper::run(&raw[1..]);
    }
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("--help");
    let rest = &args[usize::from(!args.is_empty())..];
    let arg = |index: usize| {
        rest.get(index)
            .map(String::as_str)
            .with_context(|| format!("Missing argument for {command}"))
    };
    if ["--help", "help", "-h"].contains(&command) {
        println!(
            "trailvault install [--gh-wrapper] | uninstall [--gh-wrapper] | agent add/remove copilot-cli | agent list | consent [--approve | --decline] | policy status | enable [--cloud] [--push-sessions | --skip-push-sessions] | disable | status | push [remote] | fetch [remote] | checkpoint <id> | restore <id> --session <uuid> --copilot-home <absolute-path> --working-directory <path> [--allow-lossy] [--dry-run] | recover | version"
        );
        return Ok(());
    }
    if ["version", "--version"].contains(&command) {
        println!("Trailvault CLI {VERSION}");
        return Ok(());
    }
    if command == "restore" && rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "trailvault restore <checkpoint-id> --session <source-uuid> --copilot-home <absolute-path> --working-directory <existing-path> [--allow-lossy] [--dry-run]\nExperimental Copilot CLI 1.0.105 native-file restoration. Creates a NEW local session, never overwrites the original. Does not launch Copilot or modify code. Fetch the checkpoint first. Runtime state/permissions/rewind files are not restored. Use an isolated home and only resume trusted transcripts; archived context can influence agent actions."
        );
        return Ok(());
    }
    if ["install", "uninstall", "agent"].contains(&command) {
        if rest
            .iter()
            .any(|arg| ["--help", "-h"].contains(&arg.as_str()))
        {
            println!(
                "trailvault install [--global] [--gh-wrapper] [--gh-bin /absolute/path/to/gh]\ntrailvault uninstall [--global] [--gh-wrapper]\ntrailvault agent add copilot-cli [--global]\ntrailvault agent remove copilot-cli [--global]\ntrailvault agent list\nInstall hooks once for this user. Capture is local by default; publishing requires per-repository opt-in."
            );
            return Ok(());
        }
        if ["install", "uninstall"].contains(&command)
            && rest.iter().any(|arg| arg == "--gh-wrapper")
        {
            let mut real = None;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--gh-wrapper" | "--global" => {}
                    "--gh-bin" if command == "install" => {
                        index += 1;
                        ensure!(real.is_none(), "Repeated --gh-bin");
                        real = Some(Path::new(
                            rest.get(index).context("--gh-bin requires a path")?,
                        ));
                    }
                    other => bail!("Unsupported wrapper installation option {other}"),
                }
                index += 1;
            }
            return if command == "install" {
                gh_wrapper::install(&env::current_exe()?.canonicalize()?, real)
            } else {
                gh_wrapper::uninstall()
            };
        }
        let action = if command == "agent" {
            let action = arg(0)?;
            if action == "list" {
                ensure!(rest.len() == 1, "agent list takes no arguments");
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"copilot-cli":{"installed":install::installed()?,"scope":"user","path":install::path()?}})
                    )?
                );
                return Ok(());
            }
            ensure!(arg(1)? == "copilot-cli", "Only copilot-cli is supported");
            ensure!(
                rest.len() == 2 || (rest.len() == 3 && rest[2] == "--global"),
                "Unsupported agent arguments"
            );
            action
        } else {
            ensure!(
                rest.is_empty() || rest == ["--global"],
                "Unsupported installation arguments"
            );
            if command == "install" {
                "add"
            } else {
                "remove"
            }
        };
        match action {
            "add" => install::install(&env::current_exe()?.canonicalize()?)?,
            "remove" => install::uninstall()?,
            _ => bail!("Unknown agent command {action}"),
        }
        return Ok(());
    }
    if command == "hooks"
        && rest.first().is_some_and(|arg| arg == "copilot-cli")
        && rest.iter().any(|arg| arg == "--global")
    {
        ensure!(
            rest.len() == 3 && rest[2] == "--global",
            "Expected hooks copilot-cli <event> --global"
        );
        let input = hook_input()?;
        return install::dispatch(arg(1)?, &input, &env::current_exe()?.canonicalize()?);
    }
    let repo = Repo::discover(&env::current_dir()?)?;
    match command {
        "cloud" => match arg(0)? {
            "export" => {
                ensure!(rest.len() == 2, "Use cloud export <manifest.json>");
                cloud::export(
                    &repo,
                    &env::var("GITHUB_REPOSITORY").context("GITHUB_REPOSITORY is required")?,
                    env::var("GITHUB_RUN_ID")
                        .context("GITHUB_RUN_ID is required")?
                        .parse()?,
                    env::var("GITHUB_RUN_ATTEMPT")
                        .context("GITHUB_RUN_ATTEMPT is required")?
                        .parse()?,
                    Path::new(arg(1)?),
                )?;
            }
            "upload" => {
                ensure!(rest.len() == 2, "Use cloud upload <manifest.json>");
                cloud::upload(Path::new(arg(1)?))?;
            }
            "dispatch" => {
                ensure!(rest.len() == 2, "Use cloud dispatch <manifest.json>");
                cloud::dispatch(&repo, Path::new(arg(1)?))?;
            }
            "publish" => {
                ensure!(
                    rest.len() == 6,
                    "Use cloud publish <manifest.json> <repository> <run-id> <attempt> <source-branch>"
                );
                cloud::publish(
                    &repo,
                    Path::new(arg(1)?),
                    arg(2)?,
                    arg(3)?.parse()?,
                    arg(4)?.parse()?,
                    arg(5)?,
                )?;
            }
            other => bail!("Unknown cloud command {other}"),
        },
        "restore" => {
            let checkpoint = arg(0)?;
            let mut session = None;
            let mut home = None;
            let mut work = None;
            let mut allow_lossy = false;
            let mut dry_run = false;
            let mut index = 1;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--session" | "--copilot-home" | "--working-directory" => {
                        let flag = rest[index].as_str();
                        index += 1;
                        let value = rest
                            .get(index)
                            .filter(|value| !value.starts_with("--"))
                            .with_context(|| format!("{flag} needs a value"))?;
                        let slot = match flag {
                            "--session" => &mut session,
                            "--copilot-home" => &mut home,
                            _ => &mut work,
                        };
                        ensure!(slot.is_none(), "Repeated {flag}");
                        *slot = Some(value.as_str());
                    }
                    "--allow-lossy" => {
                        ensure!(!allow_lossy, "Repeated --allow-lossy");
                        allow_lossy = true;
                    }
                    "--dry-run" => {
                        ensure!(!dry_run, "Repeated --dry-run");
                        dry_run = true;
                    }
                    other => bail!("Unsupported restore option {other}"),
                }
                index += 1;
            }
            let home = Path::new(
                home.context("Explicit --copilot-home is required; prefer an isolated directory")?,
            );
            ensure!(home.is_absolute(), "--copilot-home must be absolute");
            let options = restore::Options {
                working_directory: work
                    .context("Explicit --working-directory is required")?
                    .into(),
                allow_lossy,
            };
            let snapshot =
                restore::load(&repo, checkpoint, session.context("--session is required")?)?;
            let prepared = restore::prepare(&snapshot, &options)?;
            for warning in prepared.receipt["warnings"]
                .as_array()
                .context("Missing restoration warnings")?
            {
                eprintln!("trailvault: {}: {}", warning["code"], warning["message"]);
            }
            let mut report = prepared.receipt.clone();
            report["dry_run"] = json!(dry_run);
            report["copilot_home"] = json!(home);
            if !dry_run {
                report["session_directory"] = json!(restore::write(&prepared, home)?);
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "consent" => {
            ensure!(
                rest.is_empty() || rest == ["--approve"] || rest == ["--decline"],
                "Use trailvault consent [--approve | --decline]"
            );
            policy::review(
                &repo,
                &env::current_exe()?.canonicalize()?,
                rest.first().map(|flag| flag == "--approve"),
            )?;
        }
        "policy" => {
            ensure!(rest == ["status"], "Use trailvault policy status");
            println!("{}", serde_json::to_string_pretty(&policy::status(&repo)?)?);
        }
        "enable" => {
            let valid = [
                "--agent",
                "copilot-cli",
                "--absolute-git-hook-path",
                "--telemetry=false",
                "--skip-push-sessions",
                "--push-sessions",
                "--cloud",
            ];
            ensure!(
                rest.iter().all(|arg| valid.contains(&arg.as_str())),
                "Unsupported enable arguments: {}",
                rest.join(" ")
            );
            ensure!(
                !(rest.iter().any(|s| s == "--skip-push-sessions")
                    && rest.iter().any(|s| s == "--push-sessions")),
                "Conflicting publication flags"
            );
            policy::require_approved(&repo)?;
            capture::enable(&repo, &env::current_exe()?.canonicalize()?)?;
            if rest.iter().any(|s| s == "--skip-push-sessions") {
                let _lock = repo.lock()?;
                capture::update_local(&repo, &json!({"strategy_options":{"push_sessions":false}}))?;
            }
            if rest.iter().any(|s| s == "--push-sessions") {
                let _lock = repo.lock()?;
                capture::update_local(&repo, &json!({"strategy_options":{"push_sessions":true}}))?;
            }
            if rest.iter().any(|s| s == "--cloud") {
                let _lock = repo.lock()?;
                capture::update_local(
                    &repo,
                    &json!({"strategy_options":{"publish_on_tool_completion":true,"cloud_artifact":true}}),
                )?;
            }
            println!(
                "Trailvault enabled for Copilot CLI. Commits link context. Automatic publication: {}.",
                if capture::settings(&repo)?.pointer("/strategy_options/push_sessions")
                    == Some(&json!(false))
                {
                    "off (local capture only)"
                } else {
                    if capture::settings(&repo)?
                        .pointer("/strategy_options/publish_on_tool_completion")
                        == Some(&json!(true))
                    {
                        "on (cloud tool completion and git push publish queued transcripts)"
                    } else {
                        "on (git push publishes queued transcripts)"
                    }
                }
            );
        }
        "disable" => {
            policy::revoke(&repo)?;
            let _lock = repo.lock()?;
            capture::update_local(&repo, &json!({"enabled":false}))?;
            println!("Trailvault disabled; existing checkpoints and local logs retained.");
        }
        "status" => println!(
            "{}",
            serde_json::to_string_pretty(&capture::status(&repo)?)?
        ),
        "push" => capture::push(
            &repo,
            rest.first().map(String::as_str).unwrap_or("origin"),
            false,
        )?,
        "fetch" => capture::fetch(&repo, rest.first().map(String::as_str).unwrap_or("origin"))?,
        "checkpoint" => println!(
            "{}",
            repo.g(&[
                "show",
                &format!("{}:metadata.json", capture::checkpoint_ref(arg(0)?)?)
            ])?
        ),
        "recover" => {
            capture::post_commit(&repo)?;
            println!(
                "Recovery checked the current commit; pending uploads can be retried with trailvault push."
            );
        }
        "hooks" => match arg(0)? {
            "copilot-cli" => {
                let input = hook_input()?;
                ensure!(
                    install::EVENTS
                        .iter()
                        .any(|(_, name)| Some(*name) == rest.get(1).map(String::as_str)),
                    "Unsupported Copilot hook {}",
                    arg(1)?
                );
                if install::installed()? {
                    eprintln!(
                        "trailvault: skipped duplicate repository Copilot hook; user-wide hooks handle this session. Restart Copilot after migration."
                    );
                    return Ok(());
                }
                capture::agent_hook(&repo, arg(1)?, &input)?;
                if matches!(arg(1)?, "session-start" | "user-prompt-submitted") {
                    capture::refresh_git_hooks(&repo, &env::current_exe()?.canonicalize()?)?;
                }
            }
            "git" => match arg(1)? {
                "prepare-commit-msg" => capture::prepare_commit(
                    &repo,
                    Path::new(arg(2)?),
                    rest.get(3).map(String::as_str),
                )?,
                "post-commit" => capture::post_commit(&repo)?,
                "pre-push" => capture::push(
                    &repo,
                    rest.get(2).map(String::as_str).unwrap_or("origin"),
                    true,
                )?,
                name => bail!("Unknown Git hook {name}"),
            },
            name => bail!("Unknown hook integration {name}"),
        },
        _ => bail!("Unknown command: {command}"),
    }
    Ok(())
}

fn hook_input() -> Result<Value> {
    let mut bytes = vec![];
    io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Hook payload exceeds 16 MiB"
    );
    let input = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).context("Invalid hook JSON")?
    };
    ensure!(input.is_object(), "Hook payload must be an object");
    Ok(input)
}
