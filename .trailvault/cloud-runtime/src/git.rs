use anyhow::{Context, Result, bail};
use fs2::FileExt;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;
use wait_timeout::ChildExt;

#[derive(Debug)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn command(
    cwd: &Path,
    binary: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    let mut child = Command::new(binary)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(|| format!("Cannot start {}", binary.display()))?;
    let stdout = child.stdout.take().context("Missing child stdout")?;
    let stderr = child.stderr.take().context("Missing child stderr")?;
    let out = thread::spawn(move || read_output(stdout));
    let err = thread::spawn(move || read_output(stderr));
    let writer = input.map(|bytes| {
        let bytes = bytes.to_vec();
        let mut stdin = child.stdin.take().expect("piped stdin");
        thread::spawn(move || stdin.write_all(&bytes))
    });
    let status = child.wait_timeout(timeout)?;
    if status.is_none() {
        killpg(Pid::from_raw(i32::try_from(child.id())?), Signal::SIGKILL)?;
        child.wait()?;
    }
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    if let Some(writer) = writer {
        let result = writer
            .join()
            .map_err(|_| anyhow::anyhow!("stdin writer panicked"))?;
        if status.is_some_and(|s| s.success()) {
            result?;
        }
    }
    let status =
        status.with_context(|| format!("{} timed out after {timeout:?}", binary.display()))?;
    Ok(Output {
        success: status.success(),
        stdout: String::from_utf8(stdout).context("Command output is not UTF-8")?,
        stderr: String::from_utf8(stderr).context("Command error output is not UTF-8")?,
    })
}

fn read_output(mut reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 128 * 1024 * 1024,
        "Command output exceeds 128 MiB"
    );
    Ok(bytes)
}

pub fn run(
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    input: Option<&[u8]>,
) -> Result<Output> {
    command(
        cwd,
        Path::new("git"),
        args,
        env,
        input,
        Duration::from_secs(120),
    )
}

pub fn checked(output: Output) -> Result<String> {
    anyhow::ensure!(
        output.success,
        "Command failed: {}\n{}",
        output.stderr,
        output.stdout
    );
    Ok(output.stdout.trim_end().to_owned())
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub common: PathBuf,
    pub git_dir: PathBuf,
}

impl Repo {
    pub fn discover(cwd: &Path) -> Result<Self> {
        let g = |args: &[&str]| checked(run(cwd, args, &BTreeMap::new(), None)?);
        let root = PathBuf::from(g(&["rev-parse", "--show-toplevel"])?);
        let common = root
            .join(g(&["rev-parse", "--git-common-dir"])?)
            .canonicalize()?;
        let git_dir = root.join(g(&["rev-parse", "--git-dir"])?).canonicalize()?;
        Ok(Self {
            root,
            common,
            git_dir,
        })
    }
    pub fn g(&self, args: &[&str]) -> Result<String> {
        checked(run(&self.root, args, &BTreeMap::new(), None)?)
    }
    pub fn input(&self, args: &[&str], input: &str) -> Result<String> {
        checked(run(
            &self.root,
            args,
            &BTreeMap::new(),
            Some(input.as_bytes()),
        )?)
    }
    pub fn env(&self, args: &[&str], env: &BTreeMap<String, String>) -> Result<String> {
        checked(run(&self.root, args, env, None)?)
    }
    pub fn lock(&self) -> Result<Lock> {
        anyhow::ensure!(
            !self.common.join("trailvault.lock").exists(),
            "Legacy JavaScript lock exists; stop the old process and inspect it before removing it"
        );
        let directory = self.common.join("trailvault");
        mkdir_private(&directory)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(directory.join("operation.lock"))?;
        let start = Instant::now();
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Lock(file)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= Duration::from_secs(15) {
                        bail!("Timed out acquiring repository lock");
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.0) {
            eprintln!("trailvault: Cannot release repository lock: {error}");
        }
    }
}

pub fn optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("Cannot read JSON {}", path.display())
            })?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Cannot read {}", path.display())),
    }
}
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    optional(path)?.with_context(|| format!("Missing {}", path.display()))
}
pub fn atomic(path: &Path, content: &[u8]) -> Result<()> {
    let directory = path.parent().context("File has no parent directory")?;
    mkdir_private(directory)?;
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}
pub fn mkdir_private(directory: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    Ok(())
}
pub fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut content = serde_json::to_vec_pretty(value)?;
    content.push(b'\n');
    atomic(path, &content)
}
pub fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
