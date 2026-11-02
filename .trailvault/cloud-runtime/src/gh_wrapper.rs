use crate::{git, install, policy};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

const MARKER: &str = "# trailvault-gh-wrapper-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    version: u32,
    real_gh: PathBuf,
    sha256: String,
}

fn paths() -> Result<(PathBuf, PathBuf)> {
    let home = PathBuf::from(env::var_os("HOME").context("HOME is unset")?);
    ensure!(home.is_absolute(), "HOME must be absolute");
    for path in [
        home.join(".local"),
        home.join(".local/bin"),
        install::home()?.join("trailvault"),
    ] {
        install::ensure_plain(&path, true)?;
    }
    Ok((
        home.join(".local/bin/gh"),
        install::home()?.join("trailvault/gh-wrapper.json"),
    ))
}

fn validate(shim: &Path, manifest: &Path) -> Result<Option<Installation>> {
    install::ensure_plain(shim, false)?;
    install::ensure_plain(manifest, false)?;
    let saved: Option<Installation> = git::optional(manifest)?;
    if let Some(saved) = &saved {
        ensure!(
            saved.version == 1,
            "Unsupported gh wrapper installation version"
        );
    }
    if shim.try_exists()? {
        let saved = saved
            .as_ref()
            .context("Refusing to overwrite an unowned gh executable")?;
        ensure!(
            crate::capture::hash(fs::read(shim)?) == saved.sha256,
            "gh wrapper was modified; refusing to overwrite or remove it"
        );
    }
    Ok(saved)
}

fn marker(path: &Path) -> Result<bool> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(4096).read_to_end(&mut bytes)?;
    Ok(bytes
        .windows(MARKER.len())
        .any(|window| window == MARKER.as_bytes()))
}

fn real_binary(
    explicit: Option<&Path>,
    shim: &Path,
    saved: Option<&Installation>,
) -> Result<PathBuf> {
    let candidates: Vec<_> = if let Some(path) = explicit {
        vec![path.to_owned()]
    } else {
        saved
            .into_iter()
            .map(|saved| saved.real_gh.clone())
            .chain(
                env::split_paths(&env::var_os("PATH").unwrap_or_default())
                    .map(|directory| directory.join("gh")),
            )
            .collect()
    };
    for candidate in candidates {
        if !candidate.is_file() {
            if explicit.is_some() {
                bail!("Real gh executable does not exist: {}", candidate.display())
            }
            continue;
        }
        let candidate = candidate.canonicalize()?;
        if candidate == shim
            || candidate == env::current_exe()?.canonicalize()?
            || marker(&candidate)?
        {
            ensure!(
                explicit.is_none(),
                "Real gh cannot be the Trailvault wrapper"
            );
            continue;
        }
        ensure!(
            fs::metadata(&candidate)?.permissions().mode() & 0o111 != 0,
            "Real gh is not executable"
        );
        return Ok(candidate);
    }
    bail!("Cannot find the original gh executable; supply --gh-bin /absolute/path/to/gh")
}

fn quote(path: &Path) -> Result<String> {
    Ok(format!(
        "'{}'",
        path.to_str()
            .context("Installation path must be UTF-8")?
            .replace('\'', "'\\''")
    ))
}

pub fn install(executable: &Path, explicit: Option<&Path>) -> Result<()> {
    let (shim, manifest) = paths()?;
    let directory = manifest
        .parent()
        .context("Missing installation directory")?;
    git::mkdir_private(directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(directory.join("gh-wrapper.lock"))?;
    lock.lock_exclusive()?;
    let saved = validate(&shim, &manifest)?;
    let real = real_binary(explicit, &shim, saved.as_ref())?;
    let script = format!(
        "#!/bin/sh\n{MARKER}\nexec {} gh-wrapper {} \"$@\"\n",
        quote(&executable.canonicalize()?)?,
        quote(&real)?
    );
    install::install(executable)?;
    git::mkdir_private(shim.parent().context("Missing shim directory")?)?;
    git::atomic(&shim, script.as_bytes())?;
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))?;
    git::save(
        &manifest,
        &Installation {
            version: 1,
            real_gh: real,
            sha256: crate::capture::hash(&script),
        },
    )?;
    println!(
        "Installed gh wrapper: {}\nPut this directory FIRST on PATH, then run `hash -r` (or restart your shell):\n  export PATH={}:\"$PATH\"\n`gh repo clone` will ask about declared logging policies; all other gh commands delegate unchanged.",
        shim.display(),
        quote(shim.parent().context("Missing shim parent")?)?
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let (shim, manifest) = paths()?;
    if !manifest
        .parent()
        .context("Missing installation directory")?
        .try_exists()?
    {
        ensure!(
            !shim.try_exists()?,
            "Refusing to remove an unowned gh executable"
        );
        return install::uninstall();
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(manifest.with_file_name("gh-wrapper.lock"))?;
    lock.lock_exclusive()?;
    validate(&shim, &manifest)?;
    git::remove(&shim)?;
    git::remove(&manifest)?;
    install::uninstall()?;
    println!("Removed the gh wrapper; the original gh executable was not changed.");
    Ok(())
}

fn clone_directory(args: &[OsString]) -> Result<PathBuf> {
    let mut positionals = Vec::new();
    let mut index = 2;
    while index < args.len() {
        let argument = &args[index];
        if argument == "--" {
            break;
        }
        if argument == "--no-upstream" {
            index += 1;
            continue;
        }
        if argument == "-u" || argument == "--upstream-remote-name" {
            index += 2;
            continue;
        }
        if argument.to_str().is_some_and(|argument| {
            argument.starts_with("--upstream-remote-name=")
                || (argument.starts_with("-u") && argument.len() > 2)
        }) {
            index += 1;
            continue;
        }
        ensure!(
            !argument.as_encoded_bytes().starts_with(b"-"),
            "Unsupported gh clone option; cannot safely locate checkout: {argument:?}"
        );
        positionals.push(argument);
        index += 1;
    }
    ensure!(
        !positionals.is_empty() && positionals.len() <= 2,
        "Expected gh repo clone REPOSITORY [DIRECTORY]"
    );
    if let Some(directory) = positionals.get(1) {
        return Ok(PathBuf::from(directory));
    }
    let repository = positionals[0]
        .to_str()
        .context("Repository identifier must be UTF-8")?
        .trim_end_matches('/');
    let name = repository
        .rsplit('/')
        .next()
        .context("Missing repository name")?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    ensure!(
        !name.is_empty() && ![".", ".."].contains(&name),
        "Invalid clone directory name"
    );
    let bare = args
        .iter()
        .skip(2)
        .any(|arg| arg == "--bare" || arg == "--mirror");
    Ok(PathBuf::from(if bare {
        format!("{name}.git")
    } else {
        name.into()
    }))
}

pub fn run(args: &[OsString]) -> Result<()> {
    let real = args.first().context("Missing original gh executable")?;
    let real = Path::new(real)
        .canonicalize()
        .context("Cannot open the original gh executable")?;
    ensure!(
        real != env::current_exe()?.canonicalize()? && !marker(&real)?,
        "Refusing recursive gh wrapper delegation"
    );
    let args = &args[1..];
    let clone = args.first() == Some(&OsString::from("repo"))
        && args.get(1) == Some(&OsString::from("clone"));
    let help = args.iter().any(|arg| arg == "--help" || arg == "-h");
    if !clone || help {
        return Err(Command::new(&real).args(args).exec()).context("Cannot execute original gh");
    }
    let directory = clone_directory(args)?;
    let status = Command::new(&real)
        .args(args)
        .status()
        .context("Cannot run original gh clone")?;
    if !status.success() {
        std::process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        );
    }
    let root = directory.canonicalize()?;
    let bare = !root.join(".git").try_exists()?;
    let path = root.to_str().context("Clone directory must be UTF-8")?;
    let mut head = if bare {
        vec!["--git-dir", path]
    } else {
        vec![]
    };
    head.extend(["rev-parse", "--verify", "--quiet", "HEAD"]);
    let head = git::run(&root, &head, &Default::default(), None)?;
    if !head.success {
        ensure!(
            head.stderr.trim().is_empty(),
            "Cannot inspect cloned HEAD: {}",
            head.stderr
        );
        return Ok(());
    }
    if bare {
        let policy = git::checked(git::run(
            &root,
            &[
                "--git-dir",
                path,
                "ls-tree",
                "HEAD",
                "--",
                ".trailvault/policy.json",
            ],
            &Default::default(),
            None,
        )?)?;
        ensure!(
            policy.is_empty(),
            "Clone retained, but policy-bearing bare/mirror clones cannot configure logging consent or local write guards. Use a normal working-tree clone."
        );
        return Ok(());
    }
    let repo = git::Repo::discover(&directory).context(
        "Clone succeeded, but Trailvault could not inspect its checkout; no consent was granted",
    )?;
    ensure!(
        repo.root == directory.canonicalize()?,
        "Clone destination is not the expected working-tree root"
    );
    policy::after_clone(&repo, &env::current_exe()?.canonicalize()?)
}
