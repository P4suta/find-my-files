//! Transactional performance measurement (ADR-0013).
//!
//! Compiles happen before the environment check. Every measured command then
//! gets an idle preflight, a whole-run clock monitor, and an idle postflight.
//! Criterion comparisons use a fresh `CRITERION_HOME`, seeded only with the
//! canonical baseline, so a same-named report from an older run cannot pass the
//! gate. Baselines are recorded into a candidate and promoted only after every
//! measurement/environment check succeeds.

use crate::{cmd, fsx, paths};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::DocumentMut;

const SAMPLE_COUNT: u8 = 6;
const MONITOR_SAMPLE_COUNT: u16 = 3_600;
const MIN_PROCESSOR_PERFORMANCE: f64 = 95.0;
const MAX_PROCESSOR_TIME: f64 = 20.0;
const BASELINE_METADATA: &str = "fmf-baseline.json";
const BENCHMARK_MANIFEST: &str = include_str!("../../engine/benches/criterion-benchmarks.txt");

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    performance: f64,
    processor_time: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Summary {
    mean_performance: f64,
    mean_processor_time: f64,
    min_performance: f64,
    max_processor_time: f64,
}

#[derive(Clone, Debug)]
struct RunIdentity {
    git_commit: String,
    git_dirty: bool,
    git_dirty_sha256: String,
    cargo_lock_sha256: String,
    rustc: String,
    processor: String,
    logical_processors: String,
}

#[derive(Debug)]
struct Measurement {
    started_unix: u64,
    finished_unix: u64,
    identity: RunIdentity,
    preflight: Summary,
    during: Summary,
    postflight: Summary,
}

impl Measurement {
    fn to_json(&self, kind: &str) -> Value {
        json!({
            "schema": 1,
            "kind": kind,
            "started_unix": self.started_unix,
            "finished_unix": self.finished_unix,
            "git": {
                "commit": self.identity.git_commit,
                "dirty": self.identity.git_dirty,
                "dirty_sha256": self.identity.git_dirty_sha256,
            },
            "cargo_lock_sha256": self.identity.cargo_lock_sha256,
            "rustc": self.identity.rustc,
            "processor": self.identity.processor,
            "logical_processors": self.identity.logical_processors,
            "counters": {
                "preflight": summary_json(self.preflight),
                "during": summary_json(self.during),
                "postflight": summary_json(self.postflight),
            },
        })
    }
}

struct CounterMonitor {
    child: Child,
}

impl CounterMonitor {
    fn start() -> Result<Self> {
        let sample_count = MONITOR_SAMPLE_COUNT.to_string();
        let child = Command::new("typeperf")
            .args(counter_args())
            .args(["-si", "1", "-sc", &sample_count])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(
                "failed to start the whole-run `typeperf` monitor — performance \
                 gates require the standard Windows processor counters",
            )?;
        Ok(Self { child })
    }

    fn finish(mut self) -> Result<Summary> {
        // A finite, deliberately oversized sample count avoids shell-specific
        // signal handling. Terminating our own child after the measured command
        // preserves all rows already written to its pipe.
        let _kill_result = self.child.kill();
        let output = self
            .child
            .wait_with_output()
            .context("failed to collect the whole-run processor monitor")?;
        let samples = parse_samples(&String::from_utf8_lossy(&output.stdout))
            .context("whole-run processor monitor returned no usable samples")?;
        let summary = summarize(&samples)?;
        print_summary("perf-monitor", summary, samples.len());
        validate_clock(summary)?;
        Ok(summary)
    }
}

/// Standalone, fast environment probe used before asking an administrator to
/// run a full gate.
pub fn run() -> Result<()> {
    let _summary = sample_idle("perf-preflight")?;
    Ok(())
}

/// Compare all synthetic benchmarks in a clean run directory. The canonical
/// baseline is read-only throughout this operation.
pub fn micro_check() -> Result<()> {
    let canonical = canonical_micro_baseline_dir()?;
    compile_micro_tools()?;
    let run_dir = paths::perf_dir().join("micro-check");
    let identity = RunIdentity::capture()?;
    prepare_micro_check(&canonical, &run_dir, &identity)?;
    let criterion_home = run_dir.to_string_lossy().into_owned();
    let engine = paths::engine_dir();
    let decision_path = run_dir.join("micro-verdict.json");
    let decision_arg = decision_path.to_string_lossy().into_owned();

    let measured = measure_allow_failure(|| {
        cmd::run_env(
            &engine,
            "cargo",
            &[
                "bench",
                "--locked",
                "-p",
                "fmf-core",
                "--features",
                "testutil",
                "--bench",
                "search",
                "--",
                "--baseline",
                "committed",
            ],
            &[("CRITERION_HOME", criterion_home.as_str())],
        )
    })?;
    if let Some(error) = measured.operation_error {
        return Err(error.context("Criterion measurement failed before the verdict stage"));
    }

    let verdict_error = run_fmf(&[
        "criterion-gate",
        "--dir",
        criterion_home.as_str(),
        "--evidence",
        decision_arg.as_str(),
    ])
    .err();
    let evidence = assemble_gate_evidence(
        "micro-gate",
        "micro-verdict",
        &measured.measurement,
        &decision_path,
        verdict_error.as_ref(),
    );
    let evidence_path = run_dir.join("fmf-run.json");
    write_json(&evidence_path, &evidence)?;
    require_passing_evidence(&evidence, &evidence_path)
}

/// Record the full synthetic suite into an isolated candidate. Promotion of
/// the whole directory occurs only after the monitored run and structural
/// validation both succeed.
pub fn micro_baseline() -> Result<()> {
    let canonical = canonical_micro_baseline_dir()?;
    compile_micro_tools()?;
    let candidate = micro_baseline_candidate(&canonical)?;
    fsx::force_remove_dir_all(&candidate)
        .with_context(|| format!("failed to clear {}", candidate.display()))?;
    fs::create_dir_all(&candidate)
        .with_context(|| format!("failed to create {}", candidate.display()))?;
    let criterion_home = candidate.to_string_lossy().into_owned();
    let engine = paths::engine_dir();

    let measurement = measure(|| {
        cmd::run_env(
            &engine,
            "cargo",
            &[
                "bench",
                "--locked",
                "-p",
                "fmf-core",
                "--features",
                "testutil",
                "--bench",
                "search",
                "--",
                "--save-baseline",
                "committed",
            ],
            &[("CRITERION_HOME", criterion_home.as_str())],
        )
    })?;
    validate_micro_baseline(&candidate, "committed")?;
    validate_baseline_measurement(
        &measurement.to_json("micro-baseline"),
        "micro-baseline",
        &measurement.identity,
    )?;
    write_json(
        &candidate.join(BASELINE_METADATA),
        &measurement.to_json("micro-baseline"),
    )?;
    replace_dir_transactionally(&candidate, &canonical)
}

fn micro_baseline_candidate(canonical: &Path) -> Result<PathBuf> {
    if env::var_os("FMF_PERF_BASELINE_DIR").is_none() {
        return Ok(paths::perf_dir().join("micro-baseline-candidate"));
    }
    let mut name = canonical
        .file_name()
        .context("FMF_PERF_BASELINE_DIR must have a final path component")?
        .to_os_string();
    name.push(".candidate");
    Ok(canonical.with_file_name(name))
}

fn canonical_micro_baseline_dir() -> Result<PathBuf> {
    let configured = env::var_os("FMF_PERF_BASELINE_DIR");
    let path = paths::criterion_dir();
    if configured.is_none() {
        return Ok(path);
    }
    if path.as_os_str().is_empty() || !path.is_absolute() {
        bail!("FMF_PERF_BASELINE_DIR must be a non-empty absolute path");
    }

    let canonical = path.canonicalize().with_context(|| {
        format!(
            "FMF_PERF_BASELINE_DIR must name an existing directory: {}",
            path.display()
        )
    })?;
    if !canonical.is_dir() {
        bail!(
            "FMF_PERF_BASELINE_DIR is not a directory: {}",
            canonical.display()
        );
    }
    let repo = paths::repo_root()
        .canonicalize()
        .context("failed to resolve repository root for baseline isolation")?;
    if path_is_within(&canonical, &repo) {
        bail!(
            "FMF_PERF_BASELINE_DIR must be outside the repository checkout: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}

#[cfg(windows)]
fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = path.to_string_lossy().to_lowercase();
    let root = root.to_string_lossy().to_lowercase();
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('\\') || suffix.starts_with('/'))
}

#[cfg(not(windows))]
fn path_is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Run the real-volume acceptance gate after compilation, with continuous
/// environment validation. The committed baseline is never modified.
pub fn real_check(drive: &str) -> Result<()> {
    compile_real_tool()?;
    let baseline = paths::real_baseline();
    let identity = RunIdentity::capture()?;
    validate_real_baseline(&baseline, &identity)?;
    let baseline_arg = baseline.to_string_lossy().into_owned();
    let perf_dir = paths::perf_dir();
    fs::create_dir_all(&perf_dir)
        .with_context(|| format!("failed to create {}", perf_dir.display()))?;
    let actual = perf_dir.join("real-actual.json");
    let actual_arg = actual.to_string_lossy().into_owned();
    let decision = perf_dir.join("real-verdict.json");
    let decision_arg = decision.to_string_lossy().into_owned();

    let measured = measure_allow_failure(|| {
        run_fmf(&[
            "bench",
            drive,
            "--out",
            actual_arg.as_str(),
            "--baseline",
            baseline_arg.as_str(),
            "--evidence",
            decision_arg.as_str(),
        ])
    })?;
    let evidence = assemble_gate_evidence(
        "real-gate",
        "real-verdict",
        &measured.measurement,
        &decision,
        measured.operation_error.as_ref(),
    );
    let run_record = perf_dir.join("real-check.json");
    write_json(&run_record, &evidence)?;
    require_passing_evidence(&evidence, &run_record)
}

/// Record a real-volume candidate and atomically replace the committed JSON
/// only after the run, counter checks, and JSON validation all succeed.
pub fn real_baseline(drive: &str) -> Result<()> {
    compile_real_tool()?;
    let perf_dir = paths::perf_dir();
    fs::create_dir_all(&perf_dir)
        .with_context(|| format!("failed to create {}", perf_dir.display()))?;
    let candidate = perf_dir.join("real-baseline-candidate.json");
    remove_file_if_exists(&candidate)?;
    let candidate_arg = candidate.to_string_lossy().into_owned();

    let measurement = measure(|| run_fmf(&["bench", drive, "--out", candidate_arg.as_str()]))?;
    inject_measurement(&candidate, measurement.to_json("real-baseline"))?;
    validate_real_baseline(&candidate, &measurement.identity)?;
    replace_file_transactionally(
        &candidate,
        &paths::real_baseline(),
        &perf_dir.join("real-baseline-previous.json"),
    )
}

fn compile_micro_tools() -> Result<()> {
    let engine = paths::engine_dir();
    cmd::run(
        &engine,
        "cargo",
        &[
            "bench",
            "--locked",
            "-p",
            "fmf-core",
            "--features",
            "testutil",
            "--bench",
            "search",
            "--no-run",
        ],
    )?;
    cmd::run(
        &engine,
        "cargo",
        &["build", "--locked", "--release", "-p", "fmf-cli"],
    )
}

fn compile_real_tool() -> Result<()> {
    cmd::run(
        &paths::engine_dir(),
        "cargo",
        &["build", "--locked", "--release", "-p", "fmf-cli"],
    )
}

fn run_fmf(args: &[&str]) -> Result<()> {
    let executable = paths::engine_release_dir().join(format!("fmf{}", env::consts::EXE_SUFFIX));
    let program = executable.to_string_lossy();
    cmd::run(&paths::engine_dir(), &program, args)
}

fn measure(operation: impl FnOnce() -> Result<()>) -> Result<Measurement> {
    let measured = measure_allow_failure(operation)?;
    if let Some(error) = measured.operation_error {
        return Err(error.context("measured command failed; no baseline was promoted"));
    }
    Ok(measured.measurement)
}

struct MeasuredOperation {
    measurement: Measurement,
    operation_error: Option<anyhow::Error>,
}

fn measure_allow_failure(operation: impl FnOnce() -> Result<()>) -> Result<MeasuredOperation> {
    let identity = RunIdentity::capture()?;
    let started_unix = unix_time()?;
    let preflight = sample_idle("perf-preflight")?;
    let monitor = CounterMonitor::start()?;
    let operation_result = operation();
    let during_result = monitor.finish();
    let postflight_result = sample_idle("perf-postflight");
    let finished_unix = unix_time()?;

    if operation_result.is_err() {
        if let Err(monitor_error) = &during_result {
            eprintln!("perf-monitor also failed: {monitor_error:#}");
        }
        if let Err(postflight_error) = &postflight_result {
            eprintln!("perf-postflight also failed: {postflight_error:#}");
        }
    }

    Ok(MeasuredOperation {
        measurement: Measurement {
            started_unix,
            finished_unix,
            identity,
            preflight,
            during: during_result?,
            postflight: postflight_result?,
        },
        operation_error: operation_result.err(),
    })
}

fn assemble_gate_evidence(
    kind: &str,
    verdict_kind: &str,
    measurement: &Measurement,
    decision_path: &Path,
    operation_error: Option<&anyhow::Error>,
) -> Value {
    let mut errors = Vec::new();
    if let Some(error) = operation_error {
        errors.push(format!("{error:#}"));
    }
    let decision = match fs::read_to_string(decision_path)
        .with_context(|| format!("failed to read {}", decision_path.display()))
        .and_then(|text| {
            serde_json::from_str::<Value>(&text)
                .with_context(|| format!("invalid gate verdict {}", decision_path.display()))
        }) {
        Ok(decision) => decision,
        Err(error) => {
            errors.push(format!("{error:#}"));
            json!({
                "schema": 1,
                "kind": verdict_kind,
                "expected_cases": [],
                "cases": [],
                "finite": false,
                "passed": false,
                "errors": ["gate command did not produce a valid verdict"],
            })
        }
    };

    let valid_shape = decision.as_object().is_some_and(|object| object.len() == 7)
        && decision.get("schema").and_then(Value::as_u64) == Some(1)
        && decision.get("kind").and_then(Value::as_str) == Some(verdict_kind)
        && decision.get("expected_cases").is_some_and(Value::is_array)
        && decision.get("cases").is_some_and(Value::is_array)
        && decision.get("finite").is_some_and(Value::is_boolean)
        && decision.get("passed").is_some_and(Value::is_boolean)
        && decision.get("errors").is_some_and(Value::is_array);
    if !valid_shape {
        errors.push("gate verdict does not match schema 1".to_owned());
    }
    let complete_cases = decision
        .get("expected_cases")
        .and_then(Value::as_array)
        .zip(decision.get("cases").and_then(Value::as_array))
        .is_some_and(|(expected, cases)| {
            !expected.is_empty()
                && expected.len() == cases.len()
                && expected.iter().zip(cases).all(|(expected, case)| {
                    expected.as_str().is_some()
                        && expected.as_str() == case.get("id").and_then(Value::as_str)
                })
        });
    if valid_shape && !complete_cases {
        errors.push("gate verdict does not contain its exact ordered case set".to_owned());
    }
    if valid_shape {
        for (index, error) in decision
            .get("errors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let Some(message) = error.as_str() {
                errors.push(format!("gate verdict: {message}"));
            } else {
                errors.push(format!("gate verdict error {index} is not a string"));
            }
        }
    }
    let finite = valid_shape
        && complete_cases
        && decision.get("finite").and_then(Value::as_bool) == Some(true)
        && [
            measurement.preflight,
            measurement.during,
            measurement.postflight,
        ]
        .into_iter()
        .all(summary_is_finite);
    let decision_passed = decision.get("passed").and_then(Value::as_bool) == Some(true);
    if operation_error.is_none() && !decision_passed {
        errors.push("gate command succeeded but its verdict did not pass".to_owned());
    }
    if operation_error.is_some() && decision_passed {
        errors.push("gate command failed despite claiming a passing verdict".to_owned());
    }
    let passed = finite && decision_passed && operation_error.is_none() && errors.is_empty();

    json!({
        "schema": 1,
        "kind": kind,
        "target_sha": measurement.identity.git_commit,
        "semantic_cargo_lock_sha256": measurement.identity.cargo_lock_sha256,
        "measurement": measurement.to_json(&format!("{kind}-measurement")),
        "expected_cases": decision
            .get("expected_cases")
            .cloned()
            .unwrap_or_else(|| json!([])),
        "cases": decision
            .get("cases")
            .cloned()
            .unwrap_or_else(|| json!([])),
        "finite": finite,
        "passed": passed,
        "errors": errors,
    })
}

fn summary_is_finite(summary: Summary) -> bool {
    [
        summary.mean_performance,
        summary.mean_processor_time,
        summary.min_performance,
        summary.max_processor_time,
    ]
    .into_iter()
    .all(f64::is_finite)
}

fn require_passing_evidence(evidence: &Value, path: &Path) -> Result<()> {
    if evidence.get("passed").and_then(Value::as_bool) != Some(true) {
        bail!(
            "performance gate failed; deterministic evidence remains at {}",
            path.display()
        );
    }
    Ok(())
}

impl RunIdentity {
    fn capture() -> Result<Self> {
        let repo = paths::repo_root();
        let git_commit = checked_output(&repo, "git", &["rev-parse", "HEAD"])?;
        let (git_dirty, git_dirty_sha256) = repository_fingerprint(&repo)?;
        let cargo_lock_sha256 = semantic_cargo_lock_sha256(&repo)?;
        let rustc = checked_output(&repo, "rustc", &["--version", "--verbose"])?;
        Ok(Self {
            git_commit,
            git_dirty,
            git_dirty_sha256,
            cargo_lock_sha256,
            rustc,
            processor: env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_owned()),
            logical_processors: env::var("NUMBER_OF_PROCESSORS")
                .unwrap_or_else(|_| "unknown".to_owned()),
        })
    }
}

fn repository_fingerprint(repo: &Path) -> Result<(bool, String)> {
    let tracked = checked_command_output(repo, "git", &["diff", "--binary", "HEAD", "--", "."])?;
    let untracked = checked_command_output(
        repo,
        "git",
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;

    let mut hasher = Sha256::new();
    hasher.update(b"tracked\0");
    hasher.update(&tracked);
    hasher.update(b"untracked\0");
    hasher.update(&untracked);
    for raw_path in untracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = String::from_utf8_lossy(raw_path);
        let path = repo.join(relative.as_ref());
        if path.is_file() {
            hasher.update(raw_path);
            hasher.update(b"\0");
            hasher.update(
                fs::read(&path)
                    .with_context(|| format!("failed to fingerprint {}", path.display()))?,
            );
        }
    }

    Ok((
        !tracked.is_empty() || !untracked.is_empty(),
        hex(&hasher.finalize()),
    ))
}

fn checked_output(dir: &Path, program: &str, args: &[&str]) -> Result<String> {
    let bytes = checked_command_output(dir, program, args)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

fn checked_command_output(dir: &Path, program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to spawn `{program}`"))?;
    if !output.status.success() {
        bail!(
            "`{program} {}` exited with {}",
            args.join(" "),
            output.status
        );
    }
    Ok(output.stdout)
}

/// Hash the dependency graph while ignoring only the release-please-managed
/// versions of source-less workspace packages. A version-only Release PR must
/// not invalidate a machine baseline; registry/git sources, versions, and
/// checksums remain byte-for-byte significant.
fn semantic_cargo_lock_sha256(repo: &Path) -> Result<String> {
    let metadata = checked_output(
        repo,
        "cargo",
        &[
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
            "engine/Cargo.toml",
        ],
    )?;
    let metadata: Value =
        serde_json::from_str(&metadata).context("cargo metadata returned invalid JSON")?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .context("cargo metadata has no packages array")?;
    let workspace_packages = packages
        .iter()
        .filter(|package| package.get("source").is_some_and(Value::is_null))
        .map(|package| {
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .context("workspace package has no name")?;
            let version = package
                .get("version")
                .and_then(Value::as_str)
                .context("workspace package has no version")?;
            Ok((name.to_owned(), version.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    if workspace_packages.is_empty() {
        bail!("cargo metadata identified no source-less workspace packages");
    }

    let lock_path = paths::engine_dir().join("Cargo.lock");
    let text = fs::read_to_string(&lock_path)
        .with_context(|| format!("failed to read {}", lock_path.display()))?;
    semantic_cargo_lock_sha256_from_text(&text, &workspace_packages)
}

fn semantic_cargo_lock_sha256_from_text(
    text: &str,
    workspace_packages: &BTreeMap<String, String>,
) -> Result<String> {
    let document = text
        .parse::<DocumentMut>()
        .context("Cargo.lock is not valid TOML")?;
    let packages = document
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .context("Cargo.lock has no package table")?;
    let mut found_workspace = BTreeSet::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(toml_edit::Item::as_str)
            .context("Cargo.lock package has no name")?;
        if package.get("source").is_none() {
            if let Some(expected_version) = workspace_packages.get(name) {
                let lock_version = package
                    .get("version")
                    .and_then(toml_edit::Item::as_str)
                    .context("source-less workspace package has no version")?;
                if lock_version != expected_version {
                    bail!(
                        "Cargo.lock workspace package {name} has version {lock_version}, \
                        but cargo metadata reported {expected_version}"
                    );
                }
                found_workspace.insert(name.to_owned());
            }
        }
    }
    let expected_workspace: BTreeSet<_> = workspace_packages.keys().cloned().collect();
    if found_workspace != expected_workspace {
        bail!(
            "Cargo.lock workspace package set differs from cargo metadata: \
             lock={found_workspace:?}, metadata={expected_workspace:?}"
        );
    }

    // Keep every byte significant except the two exact version spellings that
    // release-please owns. This intentionally simple canonical form can be
    // reproduced by the hosted evidence verifier with Python's standard
    // library; no target code or package manager is executed there.
    let normalized_newlines = text.replace("\r\n", "\n");
    let marker = "[[package]]";
    let mut parts = normalized_newlines.split(marker);
    let mut canonical = parts.next().unwrap_or_default().to_owned();
    for suffix in parts {
        let mut block = format!("{marker}{suffix}");
        let parsed = block
            .parse::<DocumentMut>()
            .context("Cargo.lock package block is not valid TOML")?;
        let package = parsed
            .get("package")
            .and_then(toml_edit::Item::as_array_of_tables)
            .and_then(|packages| packages.get(0))
            .context("Cargo.lock package block has no package")?;
        let name = package
            .get("name")
            .and_then(toml_edit::Item::as_str)
            .context("Cargo.lock package block has no name")?;
        if package.get("source").is_none() {
            if let Some(version) = workspace_packages.get(name) {
                let exact = format!("version = \"{version}\"");
                let replacement = "version = \"<workspace-version>\"";
                let count = block.lines().filter(|line| *line == exact).count();
                if count != 1 {
                    bail!(
                        "Cargo.lock workspace package {name} must contain one exact version line"
                    );
                }
                block = block.replacen(&exact, replacement, 1);
            }
        }
        canonical.push_str(&block);
    }
    for (name, version) in workspace_packages {
        canonical = canonical.replace(
            &format!("\"{name} {version}\""),
            &format!("\"{name} <workspace-version>\""),
        );
    }
    Ok(hex(&Sha256::digest(canonical.as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn unix_time() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn sample_idle(label: &str) -> Result<Summary> {
    let sample_count = SAMPLE_COUNT.to_string();
    let output = Command::new("typeperf")
        .args(counter_args())
        .args(["-sc", &sample_count])
        .output()
        .context(
            "failed to run Windows `typeperf` — performance gates require the \
             standard Windows processor counters",
        )?;
    if !output.status.success() {
        bail!(
            "`typeperf` could not sample the processor counters (exit {})",
            output.status
        );
    }

    let samples = parse_samples(&String::from_utf8_lossy(&output.stdout))?;
    let summary = summarize(&samples)?;
    print_summary(label, summary, samples.len());
    validate_idle(summary)?;
    Ok(summary)
}

const fn counter_args() -> [&'static str; 2] {
    [
        r"\Processor Information(_Total)\% Processor Performance",
        r"\Processor(_Total)\% Processor Time",
    ]
}

fn print_summary(label: &str, summary: Summary, sample_count: usize) {
    println!(
        "{label}: processor performance mean {:.1}% (min {:.1}%); \
         CPU mean {:.1}% (max {:.1}%) across {sample_count} samples",
        summary.mean_performance,
        summary.min_performance,
        summary.mean_processor_time,
        summary.max_processor_time,
    );
}

fn summary_json(summary: Summary) -> Value {
    json!({
        "mean_processor_performance": summary.mean_performance,
        "min_processor_performance": summary.min_performance,
        "mean_processor_time": summary.mean_processor_time,
        "max_processor_time": summary.max_processor_time,
    })
}

/// `typeperf` emits quoted PDH-CSV rows. Counter names are localized on some
/// Windows installations, so ignore the header and parse the final two numeric
/// fields of each data row instead of matching English column names.
fn parse_samples(text: &str) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line
            .split(',')
            .map(|field| field.trim().trim_matches('"'))
            .collect();
        let Some((performance, processor_time)) = fields
            .len()
            .checked_sub(2)
            .and_then(|start| fields.get(start..))
            .filter(|tail| tail.len() == 2)
            .and_then(|tail| Some((tail[0].parse::<f64>().ok()?, tail[1].parse::<f64>().ok()?)))
        else {
            continue;
        };
        if performance.is_finite() && processor_time.is_finite() {
            samples.push(Sample {
                performance,
                processor_time,
            });
        }
    }
    if samples.is_empty() {
        bail!("typeperf returned no parseable processor samples");
    }
    Ok(samples)
}

fn summarize(samples: &[Sample]) -> Result<Summary> {
    if samples.is_empty() {
        bail!("cannot summarize an empty processor sample set");
    }
    let count = samples.len() as f64;
    Ok(Summary {
        mean_performance: samples.iter().map(|sample| sample.performance).sum::<f64>() / count,
        mean_processor_time: samples
            .iter()
            .map(|sample| sample.processor_time)
            .sum::<f64>()
            / count,
        min_performance: samples
            .iter()
            .map(|sample| sample.performance)
            .fold(f64::INFINITY, f64::min),
        max_processor_time: samples
            .iter()
            .map(|sample| sample.processor_time)
            .fold(f64::NEG_INFINITY, f64::max),
    })
}

fn validate_idle(summary: Summary) -> Result<()> {
    if summary.mean_performance < MIN_PROCESSOR_PERFORMANCE
        || summary.mean_processor_time > MAX_PROCESSOR_TIME
    {
        bail!(
            "machine is not cold and idle enough for a comparable benchmark: \
             processor performance mean {:.1}% (need >= {:.0}%), CPU mean {:.1}% \
             (need <= {:.0}%). Close CPU-heavy work, let the machine cool, and retry; \
             no baseline was changed",
            summary.mean_performance,
            MIN_PROCESSOR_PERFORMANCE,
            summary.mean_processor_time,
            MAX_PROCESSOR_TIME
        );
    }
    Ok(())
}

fn validate_clock(summary: Summary) -> Result<()> {
    if summary.mean_performance < MIN_PROCESSOR_PERFORMANCE {
        bail!(
            "processor performance fell to a {:.1}% mean during the measured run \
             (need >= {:.0}%); the result is thermally invalid and no baseline \
             was promoted",
            summary.mean_performance,
            MIN_PROCESSOR_PERFORMANCE,
        );
    }
    Ok(())
}

fn expected_micro_ids() -> BTreeSet<String> {
    micro_benchmark_ids().map(str::to_owned).collect()
}

fn micro_benchmark_ids() -> impl Iterator<Item = &'static str> {
    BENCHMARK_MANIFEST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn validate_micro_baseline(root: &Path, baseline_name: &str) -> Result<()> {
    let actual = collect_baseline_ids(root, baseline_name)?;
    let expected = expected_micro_ids();
    if actual != expected {
        let missing: Vec<_> = expected.difference(&actual).cloned().collect();
        let unexpected: Vec<_> = actual.difference(&expected).cloned().collect();
        bail!(
            "Criterion baseline is incomplete or stale: missing={missing:?}, \
             unexpected={unexpected:?}"
        );
    }

    for id in micro_benchmark_ids() {
        let baseline = root.join(id).join(baseline_name);
        for required in [
            "benchmark.json",
            "estimates.json",
            "sample.json",
            "tukey.json",
        ] {
            let path = baseline.join(required);
            if !path.is_file() {
                bail!("Criterion baseline is missing {}", path.display());
            }
        }
    }
    Ok(())
}

fn collect_baseline_ids(root: &Path, baseline_name: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    collect_baseline_ids_inner(root, root, baseline_name, &mut ids)?;
    Ok(ids)
}

fn collect_baseline_ids_inner(
    root: &Path,
    dir: &Path,
    baseline_name: &str,
    ids: &mut BTreeSet<String>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()))
        }
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if entry.file_name() == baseline_name {
            let parent = path
                .parent()
                .context("Criterion baseline directory has no parent")?;
            let relative = parent
                .strip_prefix(root)
                .context("Criterion baseline escaped its output root")?;
            let id = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            ids.insert(id);
        } else {
            collect_baseline_ids_inner(root, &path, baseline_name, ids)?;
        }
    }
    Ok(())
}

fn prepare_micro_check(canonical: &Path, run_dir: &Path, identity: &RunIdentity) -> Result<()> {
    validate_micro_baseline(canonical, "committed").with_context(|| {
        "the machine-local Criterion baseline is missing or invalid; run \
         `just bench-micro-baseline` on a cold, idle machine"
    })?;
    fsx::force_remove_dir_all(run_dir)
        .with_context(|| format!("failed to clear {}", run_dir.display()))?;
    for id in micro_benchmark_ids() {
        let source = canonical.join(id).join("committed");
        let destination = run_dir.join(id).join("committed");
        fsx::copy_dir_all(&source, &destination).with_context(|| {
            format!(
                "failed to seed Criterion baseline {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
    }
    let metadata = canonical.join(BASELINE_METADATA);
    let text = fs::read_to_string(&metadata).with_context(|| {
        format!(
            "Criterion baseline has no valid measurement identity at {}; \
             record it from a clean, cold machine with `just bench-micro-baseline`",
            metadata.display()
        )
    })?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid Criterion baseline metadata {}", metadata.display()))?;
    validate_baseline_measurement(&value, "micro-baseline", identity)?;
    fs::copy(&metadata, run_dir.join(BASELINE_METADATA))
        .context("failed to copy Criterion baseline metadata")?;
    Ok(())
}

fn validate_real_baseline(path: &Path, identity: &RunIdentity) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid real-volume baseline {}", path.display()))?;
    let object = value
        .as_object()
        .context("real-volume baseline root must be a JSON object")?;
    for field in ["volume", "entries", "queries"] {
        if !object.contains_key(field) {
            bail!(
                "real-volume baseline {} is missing `{field}`",
                path.display()
            );
        }
    }
    let measurement = object.get("measurement").with_context(|| {
        format!(
            "real-volume baseline {} has no measurement identity; record it \
             from a clean, cold machine with `just bench-baseline`",
            path.display()
        )
    })?;
    validate_baseline_measurement(measurement, "real-baseline", identity)?;
    Ok(())
}

fn validate_baseline_measurement(
    value: &Value,
    expected_kind: &str,
    identity: &RunIdentity,
) -> Result<()> {
    let object = value
        .as_object()
        .context("performance baseline measurement must be a JSON object")?;
    if object.get("schema").and_then(Value::as_u64) != Some(1) {
        bail!("performance baseline measurement must use schema 1");
    }
    if object.get("kind").and_then(Value::as_str) != Some(expected_kind) {
        bail!("performance baseline measurement kind must be `{expected_kind}`");
    }
    let git = object
        .get("git")
        .and_then(Value::as_object)
        .context("performance baseline measurement is missing `git`")?;
    if git.get("dirty").and_then(Value::as_bool) != Some(false) {
        bail!("performance baselines must be recorded from a clean repository");
    }

    for (field, current) in [
        ("cargo_lock_sha256", identity.cargo_lock_sha256.as_str()),
        ("rustc", identity.rustc.as_str()),
        ("processor", identity.processor.as_str()),
        ("logical_processors", identity.logical_processors.as_str()),
    ] {
        let recorded = object
            .get(field)
            .and_then(Value::as_str)
            .with_context(|| format!("performance baseline measurement is missing `{field}`"))?;
        if recorded != current {
            bail!(
                "performance baseline `{field}` does not match this run; \
                 recorded={recorded:?}, current={current:?}"
            );
        }
    }

    let counters = object
        .get("counters")
        .and_then(Value::as_object)
        .context("performance baseline measurement is missing `counters`")?;
    for phase in ["preflight", "during", "postflight"] {
        if !counters.get(phase).is_some_and(Value::is_object) {
            bail!("performance baseline measurement is missing `{phase}` counters");
        }
    }
    Ok(())
}

fn inject_measurement(path: &Path, measurement: Value) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut value: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid benchmark JSON {}", path.display()))?;
    value
        .as_object_mut()
        .context("benchmark JSON root must be an object")?
        .insert("measurement".to_owned(), measurement);
    write_json(path, &value)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn replace_dir_transactionally(candidate: &Path, target: &Path) -> Result<()> {
    if !candidate.is_dir() {
        bail!("baseline candidate does not exist: {}", candidate.display());
    }
    let mut backup_name = target
        .file_name()
        .context("baseline target must have a final path component")?
        .to_os_string();
    backup_name.push(".previous");
    let backup = target.with_file_name(backup_name);
    fsx::force_remove_dir_all(&backup)
        .with_context(|| format!("failed to clear {}", backup.display()))?;
    let had_target = target.is_dir();
    if had_target {
        fs::rename(target, &backup).with_context(|| {
            format!(
                "failed to preserve baseline {} as {}",
                target.display(),
                backup.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(candidate, target) {
        if had_target {
            if let Err(restore_error) = fs::rename(&backup, target) {
                bail!(
                    "failed to promote {} ({error}); restoring {} also failed \
                     ({restore_error})",
                    candidate.display(),
                    target.display()
                );
            }
        }
        return Err(error).with_context(|| {
            format!(
                "failed to promote {} to {}",
                candidate.display(),
                target.display()
            )
        });
    }
    if let Err(error) = fsx::force_remove_dir_all(&backup) {
        eprintln!(
            "warning: promoted baseline but could not remove {}: {error}",
            backup.display()
        );
    }
    Ok(())
}

fn replace_file_transactionally(candidate: &Path, target: &Path, backup: &Path) -> Result<()> {
    if !candidate.is_file() {
        bail!("baseline candidate does not exist: {}", candidate.display());
    }
    remove_file_if_exists(backup)?;
    let had_target = target.is_file();
    if had_target {
        fs::rename(target, backup).with_context(|| {
            format!(
                "failed to preserve baseline {} as {}",
                target.display(),
                backup.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(candidate, target) {
        if had_target {
            if let Err(restore_error) = fs::rename(backup, target) {
                bail!(
                    "failed to promote {} ({error}); restoring {} also failed \
                     ({restore_error})",
                    candidate.display(),
                    target.display()
                );
            }
        }
        return Err(error).with_context(|| {
            format!(
                "failed to promote {} to {}",
                candidate.display(),
                target.display()
            )
        });
    }
    if let Err(error) = remove_file_if_exists(backup) {
        eprintln!(
            "warning: promoted baseline but could not remove {}: {error:#}",
            backup.display()
        );
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        env::temp_dir().join(format!("xtask-perf-{tag}-{}", std::process::id()))
    }

    fn identity() -> RunIdentity {
        RunIdentity {
            git_commit: "0".repeat(40),
            git_dirty: false,
            git_dirty_sha256: "1".repeat(64),
            cargo_lock_sha256: "2".repeat(64),
            rustc: "rustc test".to_owned(),
            processor: "test processor".to_owned(),
            logical_processors: "8".to_owned(),
        }
    }

    fn measured() -> Measurement {
        let summary = Summary {
            mean_performance: 98.0,
            mean_processor_time: 2.0,
            min_performance: 97.0,
            max_processor_time: 4.0,
        };
        Measurement {
            started_unix: 1,
            finished_unix: 2,
            identity: identity(),
            preflight: summary,
            during: summary,
            postflight: summary,
        }
    }

    fn lockfile(
        workspace_version: &str,
        registry_version: &str,
        registry_checksum: &str,
        local_helper_version: &str,
    ) -> String {
        format!(
            r#"version = 4

[[package]]
name = "fmf-core"
version = "{workspace_version}"

[[package]]
name = "local-helper"
version = "{local_helper_version}"
dependencies = [
 "fmf-core {workspace_version}",
]

[[package]]
name = "serde"
version = "{registry_version}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{registry_checksum}"
"#,
        )
    }

    #[test]
    fn lock_fingerprint_ignores_only_workspace_package_versions() {
        let first = lockfile("0.1.0", "1.0.0", "abc", "2.0.0");
        let version_bump = lockfile("0.2.0", "1.0.0", "abc", "2.0.0");
        let workspace_v1 = BTreeMap::from([("fmf-core".to_owned(), "0.1.0".to_owned())]);
        let workspace_v2 = BTreeMap::from([("fmf-core".to_owned(), "0.2.0".to_owned())]);
        assert_eq!(
            semantic_cargo_lock_sha256_from_text(&first, &workspace_v1).unwrap(),
            semantic_cargo_lock_sha256_from_text(&version_bump, &workspace_v2).unwrap(),
            "release-only workspace versions must not invalidate a baseline"
        );

        for changed in [
            lockfile("0.1.0", "1.0.1", "abc", "2.0.0"),
            lockfile("0.1.0", "1.0.0", "def", "2.0.0"),
            lockfile("0.1.0", "1.0.0", "abc", "2.0.1"),
            first.replace(
                "registry+https://github.com/rust-lang/crates.io-index",
                "git+https://github.com/serde-rs/serde?rev=deadbeef#deadbeef",
            ),
        ] {
            assert_ne!(
                semantic_cargo_lock_sha256_from_text(&first, &workspace_v1).unwrap(),
                semantic_cargo_lock_sha256_from_text(&changed, &workspace_v1).unwrap(),
                "dependency or non-workspace path-package drift must remain significant"
            );
        }

        let wrong_identity = BTreeMap::from([("fmf-core".to_owned(), "9.9.9".to_owned())]);
        assert!(
            semantic_cargo_lock_sha256_from_text(&first, &wrong_identity).is_err(),
            "metadata/lock disagreement must fail closed"
        );
        assert_eq!(
            semantic_cargo_lock_sha256_from_text(&first, &workspace_v1).unwrap(),
            semantic_cargo_lock_sha256_from_text(&first.replace('\n', "\r\n"), &workspace_v1)
                .unwrap(),
            "semantic lock identity must not depend on checkout newline conversion"
        );
    }

    #[test]
    fn gate_envelope_binds_identity_and_cannot_pass_a_failed_command() {
        let base = scratch("gate-envelope");
        let _cleanup = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let decision = base.join("decision.json");
        write_json(
            &decision,
            &json!({
                "schema": 1,
                "kind": "micro-verdict",
                "expected_cases": ["query/common"],
                "cases": [{"id": "query/common"}],
                "finite": true,
                "passed": true,
                "errors": [],
            }),
        )
        .unwrap();

        let measurement = measured();
        let passing =
            assemble_gate_evidence("micro-gate", "micro-verdict", &measurement, &decision, None);
        assert_eq!(passing["target_sha"], measurement.identity.git_commit);
        assert_eq!(
            passing["semantic_cargo_lock_sha256"],
            measurement.identity.cargo_lock_sha256
        );
        assert_eq!(passing["passed"], true);

        let error = anyhow::anyhow!("simulated gate exit");
        let failed = assemble_gate_evidence(
            "micro-gate",
            "micro-verdict",
            &measurement,
            &decision,
            Some(&error),
        );
        assert_eq!(failed["passed"], false);
        assert!(failed["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty()));

        write_json(
            &decision,
            &json!({
                "schema": 1,
                "kind": "micro-verdict",
                "expected_cases": ["query/common"],
                "cases": [{"id": "query/common"}],
                "finite": true,
                "passed": true,
                "errors": ["simulated contradictory verdict"],
            }),
        )
        .unwrap();
        let contradictory =
            assemble_gate_evidence("micro-gate", "micro-verdict", &measurement, &decision, None);
        assert_eq!(contradictory["passed"], false);

        write_json(
            &decision,
            &json!({
                "schema": 1,
                "kind": "micro-verdict",
                "expected_cases": ["query/common", "query/other"],
                "cases": [{"id": "query/other"}, {"id": "query/common"}],
                "finite": true,
                "passed": true,
                "errors": [],
            }),
        )
        .unwrap();
        let reordered =
            assemble_gate_evidence("micro-gate", "micro-verdict", &measurement, &decision, None);
        assert_eq!(reordered["passed"], false);
        fsx::force_remove_dir_all(&base).unwrap();
    }

    fn measurement(kind: &str, identity: &RunIdentity) -> Value {
        json!({
            "schema": 1,
            "kind": kind,
            "started_unix": 1,
            "finished_unix": 2,
            "git": {
                "commit": identity.git_commit,
                "dirty": identity.git_dirty,
                "dirty_sha256": identity.git_dirty_sha256,
            },
            "cargo_lock_sha256": identity.cargo_lock_sha256,
            "rustc": identity.rustc,
            "processor": identity.processor,
            "logical_processors": identity.logical_processors,
            "counters": {
                "preflight": {},
                "during": {},
                "postflight": {},
            },
        })
    }

    fn create_complete_baseline(root: &Path, identity: &RunIdentity) {
        for id in micro_benchmark_ids() {
            let dir = root.join(id).join("committed");
            fs::create_dir_all(&dir).unwrap();
            for file in [
                "benchmark.json",
                "estimates.json",
                "sample.json",
                "tukey.json",
            ] {
                fs::write(dir.join(file), b"{}").unwrap();
            }
        }
        write_json(
            &root.join(BASELINE_METADATA),
            &measurement("micro-baseline", identity),
        )
        .unwrap();
    }

    #[test]
    fn parses_pdh_csv_without_depending_on_header_names() {
        let text = concat!(
            "\"(PDH-CSV 4.0)\",\"localized counter one\",\"localized counter two\"\n",
            "\"07/26/2026 03:26:58.882\",\"98.250000\",\"4.500000\"\n",
            "\"07/26/2026 03:26:59.887\",\"97.750000\",\"6.250000\"\n",
            "Exiting, please wait...\n",
        );
        assert_eq!(
            parse_samples(text).unwrap(),
            vec![
                Sample {
                    performance: 98.25,
                    processor_time: 4.5,
                },
                Sample {
                    performance: 97.75,
                    processor_time: 6.25,
                },
            ]
        );
    }

    #[test]
    fn rejects_header_only_or_malformed_output() {
        assert!(parse_samples("\"(PDH-CSV 4.0)\",\"counter\"\n").is_err());
        assert!(parse_samples("\"time\",\"not-a-number\",\"5.0\"\n").is_err());
    }

    #[test]
    fn summary_and_thresholds_are_exact() {
        let good = summarize(&[
            Sample {
                performance: 96.0,
                processor_time: 10.0,
            },
            Sample {
                performance: 98.0,
                processor_time: 20.0,
            },
        ])
        .unwrap();
        assert_eq!(
            good,
            Summary {
                mean_performance: 97.0,
                mean_processor_time: 15.0,
                min_performance: 96.0,
                max_processor_time: 20.0,
            }
        );
        assert!(validate_idle(good).is_ok());
        assert!(validate_clock(good).is_ok());

        assert!(validate_idle(Summary {
            mean_performance: 94.99,
            ..good
        })
        .is_err());
        assert!(validate_idle(Summary {
            mean_processor_time: 20.01,
            ..good
        })
        .is_err());
        assert!(validate_clock(Summary {
            mean_performance: 94.99,
            ..good
        })
        .is_err());
    }

    #[test]
    fn baseline_validation_and_fresh_seed_reject_stale_reports() {
        let base = scratch("seed");
        let _cleanup = fsx::force_remove_dir_all(&base);
        let canonical = base.join("canonical");
        let run_dir = base.join("run");
        let identity = identity();
        create_complete_baseline(&canonical, &identity);
        fs::create_dir_all(run_dir.join("query/common/change")).unwrap();
        fs::write(run_dir.join("query/common/change/estimates.json"), b"stale").unwrap();

        validate_micro_baseline(&canonical, "committed").unwrap();
        prepare_micro_check(&canonical, &run_dir, &identity).unwrap();
        assert!(!run_dir.join("query/common/change").exists());
        validate_micro_baseline(&run_dir, "committed").unwrap();

        fs::create_dir_all(canonical.join("old_benchmark/committed")).unwrap();
        assert!(validate_micro_baseline(&canonical, "committed").is_err());
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn directory_promotion_replaces_the_whole_baseline() {
        let base = scratch("dir-promote");
        let _cleanup = fsx::force_remove_dir_all(&base);
        let candidate = base.join("candidate");
        let target = base.join("criterion");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(candidate.join("new"), b"new").unwrap();
        fs::write(target.join("old"), b"old").unwrap();

        replace_dir_transactionally(&candidate, &target).unwrap();
        assert_eq!(fs::read(target.join("new")).unwrap(), b"new");
        assert!(!target.join("old").exists());
        assert!(!candidate.exists());
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn file_promotion_replaces_the_baseline_without_leaving_a_backup() {
        let base = scratch("file-promote");
        let _cleanup = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let candidate = base.join("candidate.json");
        let target = base.join("baseline.json");
        let backup = base.join("backup.json");
        fs::write(&candidate, b"new").unwrap();
        fs::write(&target, b"old").unwrap();

        replace_file_transactionally(&candidate, &target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!candidate.exists());
        assert!(!backup.exists());
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn measurement_is_embedded_in_real_baseline_json() {
        let base = scratch("metadata");
        let _cleanup = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let path = base.join("baseline.json");
        fs::write(&path, br#"{"volume":"C:","entries":1,"queries":[]}"#).unwrap();

        let identity = identity();
        inject_measurement(&path, measurement("real-baseline", &identity)).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["measurement"]["schema"], 1);
        validate_real_baseline(&path, &identity).unwrap();
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn baseline_identity_is_required_clean_and_machine_exact() {
        let identity = identity();
        let mut value = measurement("micro-baseline", &identity);
        validate_baseline_measurement(&value, "micro-baseline", &identity).unwrap();

        value["git"]["dirty"] = Value::Bool(true);
        assert!(
            validate_baseline_measurement(&value, "micro-baseline", &identity).is_err(),
            "a dirty canonical baseline must never be accepted"
        );

        let mut value = measurement("micro-baseline", &identity);
        value["processor"] = Value::String("another CPU".to_owned());
        assert!(
            validate_baseline_measurement(&value, "micro-baseline", &identity).is_err(),
            "cross-machine comparisons must fail closed"
        );
    }
}
