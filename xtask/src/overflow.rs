use super::{
    inject_measurement, measure_allow_failure, validate_baseline_measurement,
    validate_comparable_run, write_json, Measurement, RunIdentity, Summary,
};
use crate::{checksum, cmd, paths};
use anyhow::{bail, ensure, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(kani)]
#[path = "overflow_proofs.rs"]
mod kani_proofs;

pub(super) const ROUNDS: usize = 5;
const VARIANTS: [Variant; 3] = [Variant::A, Variant::B, Variant::C];
const SCHEMA: u32 = 1;
const COOLING_SECONDS: u64 = 60;
const GENERIC_FAILURE: i32 = 1;
const AFFINITY_HEX_RADIX: u32 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Mode {
    Plan,
    Prepare,
    Measure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum Variant {
    A,
    B,
    C,
}

impl Variant {
    const fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    variant: Variant,
    arguments: Vec<String>,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    schema: u32,
    finished_unix_ms: u64,
    source: RunIdentity,
    candidates: Vec<Candidate>,
}

struct Recorded {
    variant: Variant,
    report: PathBuf,
    measurement: Measurement,
}

pub(super) const fn sequence() -> [[Variant; 3]; ROUNDS] {
    [
        [Variant::A, Variant::B, Variant::C],
        [Variant::C, Variant::B, Variant::A],
        [Variant::A, Variant::B, Variant::C],
        [Variant::C, Variant::B, Variant::A],
        [Variant::A, Variant::B, Variant::C],
    ]
}

pub(super) const fn build_args(variant: Variant) -> &'static [&'static str] {
    match variant {
        Variant::A => &["build", "--locked", "--release", "-p", "fmf-cli"],
        Variant::B => &[
            "build",
            "--locked",
            "--release",
            "-p",
            "fmf-cli",
            "--config",
            "profile.release.package.fmf-core.overflow-checks=true",
            "--config",
            "profile.release.package.fmf-runtime.overflow-checks=true",
        ],
        Variant::C => &[
            "build",
            "--locked",
            "--release",
            "-p",
            "fmf-cli",
            "--config",
            "profile.release.overflow-checks=true",
        ],
    }
}

pub(super) const fn boot_follows_preparation(prepared: u64, boot: u64, now: u64) -> bool {
    prepared < boot && boot <= now
}

pub fn run(mode: Mode) -> Result<()> {
    match mode {
        Mode::Plan => plan(),
        Mode::Prepare => prepare(),
        Mode::Measure => measure(),
    }
}

fn plan() -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "rounds": sequence(),
            "build_arguments": VARIANTS.map(build_args),
            "drive": "C:",
            "output": paths::overflow_experiment_dir(),
            "cooling_seconds_between_runs": COOLING_SECONDS,
            "measurement_requires_new_boot_after_build": true,
            "profile_expansion_authorized": false,
        }))?
    );
    Ok(())
}

fn unix_millis() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch")?
        .as_millis()
        .try_into()
        .context("system clock exceeds the experiment timestamp range")
}

fn boot_millis() -> Result<u64> {
    let output = cmd::capture_stdout(
        &paths::repo_root(),
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "([DateTimeOffset]((Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime())).ToUnixTimeMilliseconds().ToString([Globalization.CultureInfo]::InvariantCulture)",
        ],
    )?;
    output
        .trim()
        .parse()
        .context("Windows returned an invalid boot timestamp")
}

fn process_affinity() -> Result<u64> {
    let command = format!(
        "[Diagnostics.Process]::GetProcessById({}).ProcessorAffinity.ToInt64().ToString('X16', [Globalization.CultureInfo]::InvariantCulture)",
        std::process::id()
    );
    let output = cmd::capture_stdout(
        &paths::repo_root(),
        "powershell.exe",
        &["-NoProfile", "-Command", &command],
    )?;
    u64::from_str_radix(output.trim(), AFFINITY_HEX_RADIX)
        .context("Windows returned an invalid processor affinity")
}

fn require_default_environment() -> Result<()> {
    for (key, _) in env::vars_os() {
        let key = key.to_string_lossy().to_ascii_uppercase();
        let compiler_override = matches!(
            key.as_str(),
            "RUSTFLAGS"
                | "RUSTC"
                | "RUSTC_WRAPPER"
                | "RUSTC_WORKSPACE_WRAPPER"
                | "CARGO_ENCODED_RUSTFLAGS"
                | "CARGO_BUILD_RUSTFLAGS"
                | "CARGO_BUILD_TARGET"
                | "CARGO_TARGET_DIR"
        ) || key.starts_with("CARGO_PROFILE_")
            || (key.starts_with("CARGO_TARGET_") && key.ends_with("_RUSTFLAGS"));
        ensure!(
            !compiler_override,
            "remove the ambient {key} override before the experiment"
        );
    }
    Ok(())
}

fn executable(output: &Path, variant: Variant) -> PathBuf {
    output.join(format!("{}.exe", variant.label()))
}

fn prepare() -> Result<()> {
    ensure!(cfg!(windows), "overflow preparation requires Windows");
    require_default_environment()?;
    let output = paths::overflow_experiment_dir();
    let manifest = output.join("prepared.json");
    ensure!(
        !manifest.exists(),
        "archive the previous experiment before preparing another one"
    );
    let source = RunIdentity::capture()?;
    ensure!(
        !source.git_dirty,
        "commit the source before preparing the comparison binaries"
    );
    fs::create_dir_all(&output)?;
    let mut candidates = Vec::new();
    for variant in VARIANTS {
        let arguments = build_args(variant);
        cmd::run(&paths::engine_dir(), "cargo", arguments)?;
        let target = executable(&output, variant);
        fs::copy(paths::engine_release_dir().join("fmf.exe"), &target)
            .with_context(|| format!("preserve variant {}", variant.label()))?;
        candidates.push(Candidate {
            variant,
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            sha256: checksum::sha256_hex(&fs::read(&target)?),
        });
    }
    cmd::run(&paths::engine_dir(), "cargo", build_args(Variant::A))?;
    ensure!(
        source == RunIdentity::capture()?,
        "source changed while preparing the comparison"
    );
    let prepared = Prepared {
        schema: SCHEMA,
        finished_unix_ms: unix_millis()?,
        source,
        candidates,
    };
    write_json(&manifest, &serde_json::to_value(prepared)?)?;
    println!("Prepared three comparison binaries: {}", manifest.display());
    println!("Measurement requires an owner-controlled restart after preparation.");
    Ok(())
}

fn require_prepared(prepared: &Prepared, output: &Path) -> Result<()> {
    require_default_environment()?;
    ensure!(
        prepared.schema == SCHEMA,
        "unsupported overflow experiment schema"
    );
    ensure!(!prepared.source.git_dirty, "the prepared source was dirty");
    let recorded_processors: std::num::NonZeroUsize = prepared
        .source
        .logical_processors
        .parse()
        .context("the prepared logical processor count is invalid")?;
    ensure!(
        usize::try_from(process_affinity()?.count_ones())? == recorded_processors.get(),
        "measurement requires the processor affinity recorded at preparation"
    );
    ensure!(
        prepared.source == RunIdentity::capture()?,
        "prepared source or machine identity changed"
    );
    ensure!(
        prepared.candidates.len() == VARIANTS.len(),
        "incomplete comparison binaries"
    );
    for (candidate, expected) in prepared.candidates.iter().zip(VARIANTS) {
        ensure!(
            candidate.variant == expected,
            "comparison variants are missing or duplicated"
        );
        ensure!(
            candidate
                .arguments
                .iter()
                .map(String::as_str)
                .eq(build_args(expected).iter().copied()),
            "comparison profile arguments changed"
        );
        let actual = checksum::sha256_hex(&fs::read(executable(output, expected))?);
        ensure!(candidate.sha256 == actual, "comparison executable changed");
    }
    ensure!(
        boot_follows_preparation(prepared.finished_unix_ms, boot_millis()?, unix_millis()?),
        "measurement requires a new boot after preparation and a consistent system clock"
    );
    Ok(())
}

fn compare(output: &Path, label: &str, actual: &Path, baseline: &Path) -> Result<Value> {
    let evidence = output.join(format!("{label}-verdict.json"));
    let status = Command::new(executable(output, Variant::A))
        .arg("bench-compare")
        .arg("--actual")
        .arg(actual)
        .arg("--baseline")
        .arg(baseline)
        .arg("--evidence")
        .arg(&evidence)
        .current_dir(paths::engine_dir())
        .status()
        .context("run the unchanged real-volume evaluator")?;
    let decision: Value = serde_json::from_slice(&fs::read(&evidence)?)?;
    ensure!(
        decision["kind"] == "real-verdict" && decision["finite"] == true,
        "the comparison did not produce a finite gate decision"
    );
    let cases = decision["cases"]
        .as_array()
        .context("missing comparison cases")?;
    let expected = decision["expected_cases"]
        .as_array()
        .context("missing comparison inventory")?;
    ensure!(
        !expected.is_empty() && cases.len() == expected.len(),
        "incomplete comparison cases"
    );
    let passed = decision["passed"]
        .as_bool()
        .context("missing comparison verdict")?;
    ensure!(
        status.code() == Some(if passed { 0 } else { GENERIC_FAILURE }),
        "the comparison exit status disagrees with its verdict"
    );
    Ok(decision)
}

fn record_run(
    output: &Path,
    prepared: &Prepared,
    round: usize,
    variant: Variant,
) -> Result<Recorded> {
    require_prepared(prepared, output)?;
    let label = format!("round-{round}-{}", variant.label());
    let report = output.join(format!("{label}.json"));
    ensure!(!report.exists(), "a report already exists for {label}");
    let measured = measure_allow_failure(|| {
        let status = Command::new(executable(output, variant))
            .args(["bench", "C:", "--out"])
            .arg(&report)
            .current_dir(paths::engine_dir())
            .status()
            .context("run the prepared real-volume benchmark")?;
        ensure!(status.success(), "benchmark exited with {status}");
        Ok(())
    })?;
    if let Some(error) = measured.operation_error {
        return Err(error.context(format!("benchmark {label} failed")));
    }
    let measurement = measured.measurement.to_json("overflow-run");
    validate_baseline_measurement(&measurement, "overflow-run", &prepared.source)?;
    inject_measurement(&report, measurement)?;
    require_prepared(prepared, output)?;
    Ok(Recorded {
        variant,
        report,
        measurement: measured.measurement,
    })
}

fn measure_all(output: &Path, prepared: &Prepared, records: &mut Vec<Value>) -> Result<bool> {
    let mut first_a: Option<(PathBuf, Summary)> = None;
    let mut completed = 0;
    let mut all_passed = true;
    for (index, order) in sequence().into_iter().enumerate() {
        let round = index + 1;
        let mut measured = Vec::new();
        for variant in order {
            println!("Measuring round {round}, variant {}.", variant.label());
            let run = record_run(output, prepared, round, variant)?;
            records.push(json!({
                "round": round,
                "variant": variant,
                "measurement": run.measurement.to_json("overflow-run"),
                "report_sha256": checksum::sha256_hex(&fs::read(&run.report)?),
            }));
            measured.push(run);
            completed += 1;
            if completed < ROUNDS * VARIANTS.len() {
                println!("Cooling for {COOLING_SECONDS} seconds before the next cold preflight.");
                std::thread::sleep(Duration::from_secs(COOLING_SECONDS));
            }
        }
        let control = measured
            .iter()
            .find(|run| run.variant == Variant::A)
            .context("the round has no A control")?;
        let (first_report, first_summary) =
            first_a.get_or_insert_with(|| (control.report.clone(), control.measurement.during));
        validate_comparable_run(*first_summary, control.measurement.during)?;
        let null = compare(
            output,
            &format!("round-{round}-A-control"),
            &control.report,
            first_report,
        )?;
        all_passed &= null["passed"] == true;
        for actual in &measured {
            validate_comparable_run(control.measurement.during, actual.measurement.during)?;
            let label = format!("round-{round}-{}", actual.variant.label());
            let decision = compare(output, &label, &actual.report, &control.report)?;
            all_passed &= decision["passed"] == true;
        }
    }
    Ok(all_passed)
}

fn measure() -> Result<()> {
    ensure!(cfg!(windows), "overflow measurement requires Windows");
    let output = paths::overflow_experiment_dir();
    let prepared: Prepared = serde_json::from_slice(&fs::read(output.join("prepared.json"))?)?;
    require_prepared(&prepared, &output)?;
    let receipt = output.join("experiment.json");
    ensure!(
        !receipt.exists(),
        "an experiment receipt already exists; archive it before another run"
    );
    let mut records = Vec::new();
    let result = measure_all(&output, &prepared, &mut records);
    let error = result.as_ref().err().map(|error| format!("{error:#}"));
    write_json(
        &receipt,
        &json!({
            "schema": SCHEMA,
            "prepared": prepared,
            "rounds": sequence(),
            "expected_runs": ROUNDS * VARIANTS.len(),
            "complete": result.is_ok(),
            "all_performance_gates_passed": result.as_ref().is_ok_and(|passed| *passed),
            "profile_expansion_authorized": false,
            "runs": records,
            "error": error,
        }),
    )?;
    let passed = result?;
    if !passed {
        bail!(
            "five valid rounds were collected, but a performance gate failed; inspect {}",
            receipt.display()
        );
    }
    println!("Five valid rounds passed the existing gates; review paired costs before any profile expansion.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{boot_follows_preparation, build_args, sequence, Variant};

    #[test]
    fn measurement_requires_a_new_boot_after_preparation_without_future_timestamps() {
        assert!(boot_follows_preparation(10, 11, 11));
        assert!(boot_follows_preparation(10, 11, 12));
        assert!(!boot_follows_preparation(10, 10, 11));
        assert!(!boot_follows_preparation(10, 9, 11));
        assert!(!boot_follows_preparation(10, 12, 11));
        assert!(!boot_follows_preparation(u64::MAX, 0, u64::MAX));
    }

    #[test]
    fn variants_change_only_the_intended_release_profile_flags() {
        let baseline = vec!["build", "--locked", "--release", "-p", "fmf-cli"];
        assert_eq!(build_args(Variant::A), baseline.as_slice());
        let mut runtime = baseline.clone();
        runtime.extend([
            "--config",
            "profile.release.package.fmf-core.overflow-checks=true",
            "--config",
            "profile.release.package.fmf-runtime.overflow-checks=true",
        ]);
        assert_eq!(build_args(Variant::B), runtime.as_slice());
        let mut workspace = baseline;
        workspace.extend(["--config", "profile.release.overflow-checks=true"]);
        assert_eq!(build_args(Variant::C), workspace.as_slice());
    }

    #[test]
    fn five_rounds_alternate_forward_and_reverse_order_with_every_variant() {
        let actual = sequence();
        assert_eq!(actual.len(), 5);
        for (index, round) in actual.iter().enumerate() {
            let expected = if index % 2 == 0 {
                [Variant::A, Variant::B, Variant::C]
            } else {
                [Variant::C, Variant::B, Variant::A]
            };
            assert_eq!(*round, expected, "measurement round {index}");
        }
    }
}
