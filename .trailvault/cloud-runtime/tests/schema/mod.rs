mod collect;
mod compare;
mod corpus;

use anyhow::{Context, Result, ensure};
use libtest_mimic::{Arguments, Trial};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

const REFERENCE_VERSION: &str = "Entire CLI 0.12.2";

pub const CHECKS: [&str; 20] = [
    "artifact inventory and Git modes",
    "root metadata field sets",
    "root metadata scalar and container types",
    "root metadata normalized values",
    "session metadata field sets",
    "session metadata scalar and container types",
    "session metadata normalized values",
    "native records field sets and types",
    "native records values and order",
    "native transcript exact bytes",
    "compact wire bytes (version normalized)",
    "compact recursive field sets",
    "compact scalar and container types",
    "compact normalized values",
    "prompt exact bytes",
    "content hash encoding",
    "content hash bytes and digest integrity",
    "auxiliary JSON schema",
    "auxiliary wire bytes (task times normalized)",
    "checkpoint boundaries and artifact references",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Content {
    Text { utf8: String },
    Binary { binary: Vec<u8> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blob {
    mode: String,
    #[serde(flatten)]
    content: Content,
}

impl Blob {
    fn new(mode: &str, bytes: Vec<u8>) -> Self {
        let content = match String::from_utf8(bytes) {
            Ok(utf8) => Content::Text { utf8 },
            Err(error) => Content::Binary {
                binary: error.into_bytes(),
            },
        };
        Self {
            mode: mode.into(),
            content,
        }
    }

    fn bytes(&self) -> &[u8] {
        match &self.content {
            Content::Text { utf8 } => utf8.as_bytes(),
            Content::Binary { binary } => binary,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    id: String,
    files: BTreeMap<String, Blob>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    checkpoints: Vec<Checkpoint>,
    native: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    sha256: String,
    checkpoints: usize,
    artifacts: usize,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    reference_version: String,
    reference_binary_sha256: String,
    source_commit: String,
    recorded_at: String,
    corpus_sha256: String,
    cases: Vec<Entry>,
}

fn project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn oracle_dir() -> PathBuf {
    project().join("tests/schema/oracles")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn corpus_digest(cases: &[corpus::Case]) -> Result<String> {
    Ok(digest(&serde_json::to_vec(cases)?))
}

fn comparison_digest() -> String {
    digest(
        &[
            include_bytes!("mod.rs").as_slice(),
            include_bytes!("compare.rs").as_slice(),
            include_bytes!("collect.rs").as_slice(),
        ]
        .concat(),
    )
}

pub fn record() -> Result<()> {
    let tool = collect::Tool::new("entire")?;
    let version = trailvault::git::checked(trailvault::git::command(
        &project(),
        &tool.binary,
        &["version"],
        &BTreeMap::new(),
        None,
        std::time::Duration::from_secs(30),
    )?)?;
    ensure!(
        version.lines().next() == Some(REFERENCE_VERSION),
        "Refusing to record a different reference: {version}"
    );
    let cases = corpus::cases();
    let mut entries = vec![];
    let mut outputs = vec![];
    for (index, case) in cases.iter().enumerate() {
        eprintln!("Recording Entire oracle {}/50: {}", index + 1, case.name);
        let run = collect::collect(&tool, case)
            .with_context(|| format!("Oracle collection failed: {}", case.name))?;
        // All twenty selectors must accept the reference itself. Never record
        // a setup failure or a malformed checkpoint as a successful oracle.
        for (check, description) in CHECKS.iter().enumerate() {
            compare::check(check, &run, &run)
                .with_context(|| format!("Invalid reference {}: {}", case.name, description))?;
        }
        let bytes = serde_json::to_vec_pretty(&run)?;
        entries.push(Entry {
            name: case.name.clone(),
            sha256: digest(&bytes),
            checkpoints: run.checkpoints.len(),
            artifacts: run
                .checkpoints
                .iter()
                .map(|checkpoint| checkpoint.files.len())
                .sum(),
        });
        outputs.push((case.name.clone(), bytes));
    }
    // Collect everything before replacing any oracle; the manifest is written
    // last, and its per-file hashes reject interrupted or manual replacements.
    fs::create_dir_all(oracle_dir())?;
    for (name, bytes) in outputs {
        fs::write(oracle_dir().join(format!("{name}.json")), bytes)?;
    }
    let manifest = Manifest {
        format_version: 1,
        reference_version: version,
        reference_binary_sha256: tool.sha256()?,
        source_commit: "c4f103d8dfed1f1549db9b2c793f2d7e10f65eb8".into(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        corpus_sha256: corpus_digest(&cases)?,
        cases: entries,
    };
    fs::write(
        oracle_dir().join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn load(cases: &[corpus::Case]) -> Result<Vec<Run>> {
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(oracle_dir().join("manifest.json")).context("Missing recorded Entire oracles")?,
    )?;
    ensure!(manifest.format_version == 1, "Unsupported oracle format");
    ensure!(
        manifest.reference_version.lines().next() == Some(REFERENCE_VERSION),
        "Unexpected oracle version"
    );
    ensure!(
        manifest.corpus_sha256 == corpus_digest(cases)?,
        "Corpus changed: regenerate and revalidate the reference explicitly"
    );
    ensure!(
        manifest.cases.len() == cases.len(),
        "Oracle count does not match corpus"
    );
    let names: BTreeSet<_> = cases.iter().map(|case| &case.name).collect();
    ensure!(names.len() == 50, "Duplicate or missing fixtures");
    cases
        .iter()
        .zip(&manifest.cases)
        .map(|(case, entry)| {
            ensure!(entry.name == case.name, "Oracle order/name mismatch");
            let bytes = fs::read(oracle_dir().join(format!("{}.json", case.name)))?;
            ensure!(
                digest(&bytes) == entry.sha256,
                "Oracle checksum mismatch: {}",
                case.name
            );
            let run: Run = serde_json::from_slice(&bytes)?;
            ensure!(
                run.checkpoints.len() == entry.checkpoints,
                "Checkpoint count mismatch in manifest"
            );
            ensure!(
                run.checkpoints
                    .iter()
                    .map(|checkpoint| checkpoint.files.len())
                    .sum::<usize>()
                    == entry.artifacts,
                "Artifact count mismatch in manifest"
            );
            Ok(run)
        })
        .collect()
}

pub fn run() -> Result<()> {
    let mut arguments = Arguments::from_args();
    if arguments.test_threads.is_none() {
        arguments.test_threads = Some(1);
    }
    let cases = corpus::cases();
    let expected = load(&cases)?;
    let target = env::var("SCHEMA_ALIGNMENT_TARGET").unwrap_or_else(|_| "trailvault".into());
    let tool = collect::Tool::new(&target)?;
    let results = Arc::new(Mutex::new(Vec::<Value>::new()));
    let output_dir = project().join(".test-results/schema").join(&target);
    fs::create_dir_all(&output_dir)?;
    let mut trials = vec![];
    for (case, expected) in cases.into_iter().zip(expected) {
        let actual: Arc<OnceLock<Result<Run, String>>> = Arc::new(OnceLock::new());
        let expected = Arc::new(expected);
        let case = Arc::new(case);
        for (index, description) in CHECKS.iter().enumerate() {
            let tool = tool.clone();
            let case = case.clone();
            let actual = actual.clone();
            let expected = expected.clone();
            let results = results.clone();
            let output_dir = output_dir.clone();
            let name = format!("{}/{}", case.name, description);
            trials.push(Trial::test(name.clone(), move || {
                let collected = actual.get_or_init(|| {
                    let artifact = output_dir.join(format!("{}.json", case.name));
                    match fs::remove_file(&artifact) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("Cannot clear stale candidate artifact: {error}"));
                        }
                    }
                    let run =
                        collect::collect(&tool, &case).map_err(|error| format!("{error:#}"))?;
                    let bytes =
                        serde_json::to_vec_pretty(&run).map_err(|error| error.to_string())?;
                    fs::write(artifact, bytes).map_err(|error| error.to_string())?;
                    Ok(run)
                });
                let (kind, result) = match collected {
                    Ok(run) => (
                        "schema_mismatch",
                        compare::check(index, &expected, run).map_err(|error| format!("{error:#}")),
                    ),
                    Err(error) => ("collection_error", Err(error.clone())),
                };
                results.lock().expect("Schema results mutex").push(json!({
                    "name":name,"fixture":case.name,"check":description,"passed":result.is_ok(),
                    "failure_kind":if result.is_err() {Some(kind)} else {None},
                    "error":result.as_ref().err(),
                }));
                result.map_err(Into::into)
            }));
        }
    }
    ensure!(
        trials.len() == 1000,
        "Schema suite must contain exactly 1000 named tests"
    );
    let conclusion = libtest_mimic::run(&arguments, trials);
    let records = results.lock().expect("Schema results mutex");
    let passed = records
        .iter()
        .filter(|record| record["passed"] == true)
        .count();
    let errors = records
        .iter()
        .filter(|record| record["failure_kind"] == "collection_error")
        .count();
    let report = json!({
        "target":target,"reference":REFERENCE_VERSION,"corpus_sha256":corpus_digest(&corpus::cases())?,
        "target_binary_sha256":tool.sha256()?,
        "comparison_sha256":comparison_digest(),
        "reference_manifest_sha256":digest(&fs::read(oracle_dir().join("manifest.json"))?),
        "timestamp":chrono::Utc::now().to_rfc3339(),"fixtures":50,"planned_tests":1000,
        "executed_tests":records.len(),"passed":passed,"failed":records.len()-passed,
        "collection_error_tests":errors,"schema_mismatch_tests":records.len()-passed-errors,
        "complete":records.len()==1000,"results":*records,
    });
    fs::create_dir_all(project().join(".test-results"))?;
    fs::write(
        project().join(format!(".test-results/{target}-schema-alignment.json")),
        serde_json::to_vec_pretty(&report)?,
    )?;
    if env::var("SCHEMA_RECORD_EVIDENCE").as_deref() == Ok("1") {
        ensure!(
            records.len() == 1000,
            "Refusing to record partial schema evidence"
        );
        fs::write(
            project().join(format!("tests/schema/evidence-{target}.json")),
            serde_json::to_vec_pretty(&report)?,
        )?;
    }
    conclusion.exit();
}
