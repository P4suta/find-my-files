//! Exact-identity mutation-testing gates (ADR-0022).
//!
//! Mutation scores are deliberately not policy: a score can stay flat while a
//! critical old survivor disappears and a new security-relevant survivor takes
//! its place. These gates run each tool's unmutated baseline, parse its
//! machine-readable report, canonicalize exact mutant identities, and compare
//! the survivor set with a reviewed, rationale-bearing baseline.

use crate::{fsx, paths};
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::{
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::LazyLock;

const BASELINE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const CARGO_MUTANTS_NAME: &str = "cargo-mutants";
const CARGO_MUTANTS_VERSION: &str = "27.1.0";
const STRYKER_NAME: &str = "dotnet-stryker";
const STRYKER_VERSION: &str = "4.16.0";

/// The app-project build profile every C# mutation run must use, as environment
/// variables (`MSBuild` reads the environment as properties).
///
/// `FindMyFiles.Tests.csproj` carries these on its `ProjectReference` as
/// `AdditionalProperties`, which is enough for `dotnet test` but **not** for
/// Stryker: Stryker analyses `FindMyFiles.csproj` on its own (Buildalyzer,
/// standalone) to build the mutated compilation, so those reference-scoped
/// properties are simply absent. The app then compiles with `FmfTestSeams=false`
/// — no `InternalsVisibleTo("FindMyFiles.Tests")`, and `FakeEngineClient.cs`
/// removed from the compile — and the injected assembly makes the test host throw
/// `TypeLoadException: Access is denied: 'FindMyFiles.ViewModels.SearchRequest'`
/// during discovery. Zero tests found, every mutant Timeout.
///
/// Stryker has no `-p:` passthrough, so the environment is the only channel. It
/// lives here, on the command the gate itself spawns, rather than in a shell or a
/// workflow `env:` block: there is no step for an operator or a YAML author to
/// forget. `mutation_ci::dotnet_command` sets the identical pair for the same
/// reason, so both entry points build one app profile.
const CSHARP_TEST_PROFILE: [(&str, &str); 2] =
    [("FmfTestSeams", "true"), ("FmfArtifactKind", "ui-test")];

/// [`CSHARP_TEST_PROFILE`] for the CI shard runner, so neither entry point can
/// carry its own copy of the pair.
pub const fn csharp_test_profile() -> [(&'static str, &'static str); 2] {
    CSHARP_TEST_PROFILE
}

/// Where cargo-mutants actually writes its reports for a given `--output` dir.
///
/// `--output <D>` is the *parent*: cargo-mutants 27.1.0 creates `<D>/mutants.out`
/// (rotating any previous one to `mutants.out.old`) and puts `outcomes.json`,
/// `mutants.json`, the `*.txt` verdict lists and the per-mutant logs there. Both
/// callers — the local gate and the CI shard runner — need the same mapping, and
/// having each spell it out is exactly how they drifted: the CI path joined
/// `mutants.out` while the local path read `<D>/outcomes.json` and failed with
/// `os error 2` after a complete run. One function, pinned by a test, next to the
/// tool version it is a fact about.
pub fn cargo_mutants_report_dir(output: &Path) -> PathBuf {
    output.join("mutants.out")
}

static RUST_IDENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<path>.+?):(?P<line>[1-9][0-9]*)(?::(?P<column>[0-9]+))?: (?P<mutation>.+)$")
        .expect("the cargo-mutants identity regex is a constant")
});

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolPin {
    name: String,
    version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptedEquivalent<T> {
    identity: T,
    rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AcceptedBaseline<T> {
    schema_version: u32,
    tool: ToolPin,
    examined_files: Vec<String>,
    accepted_equivalents: Vec<AcceptedEquivalent<T>>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct RustIdentity {
    path: String,
    line: u64,
    column: Option<u64>,
    mutation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CsharpIdentity {
    pub path: String,
    pub start_line: u64,
    pub start_column: u64,
    pub end_line: u64,
    pub end_column: u64,
    pub mutator: String,
    pub replacement: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsharpReviewedPolicy {
    pub examined_files: Vec<String>,
    pub accepted_equivalents: Vec<CsharpIdentity>,
}

#[derive(Debug, Serialize)]
struct RustEvidence<'a> {
    schema_version: u32,
    tool: ToolPin,
    baseline_passed: bool,
    examined_files: &'a [String],
    survivors: &'a [RustIdentity],
    timeouts: &'a [RustIdentity],
}

#[derive(Debug, Serialize)]
struct CsharpEvidence<'a> {
    schema_version: u32,
    tool: ToolPin,
    baseline_passed: bool,
    process_exit_code: Option<i32>,
    examined_files: &'a [String],
    survivors: &'a [CsharpIdentity],
    no_coverage: &'a [CsharpIdentity],
    timeouts: &'a [CsharpIdentity],
    ignored: &'a [CsharpIdentity],
    redundant_ignored: &'a [CsharpStatusIdentity],
    outside_scope: &'a OutsideScopeSummary,
    outside_scope_violations: &'a [CsharpStatusIdentity],
}

#[derive(Debug)]
struct RustRun {
    examined_files: Vec<String>,
    survivors: Vec<RustIdentity>,
    timeouts: Vec<RustIdentity>,
}

#[derive(Debug)]
struct CsharpRun {
    examined_files: Vec<String>,
    survivors: Vec<CsharpIdentity>,
    no_coverage: Vec<CsharpIdentity>,
    timeouts: Vec<CsharpIdentity>,
    ignored: Vec<CsharpIdentity>,
    redundant_ignored: Vec<CsharpStatusIdentity>,
    outside_scope: OutsideScopeSummary,
    outside_scope_violations: Vec<CsharpStatusIdentity>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct CsharpStatusIdentity {
    identity: CsharpIdentity,
    status: String,
    status_reason: Option<String>,
}

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
struct OutsideScopeSummary {
    file_count: usize,
    mutant_count: usize,
    status_counts: BTreeMap<String, usize>,
}

pub fn run_rust() -> Result<()> {
    verify_cargo_mutants_version()?;

    let output = paths::rust_mutation_dir();
    fsx::force_remove_dir_all(&output)
        .with_context(|| format!("remove stale Rust mutation output {}", output.display()))?;
    // cargo-mutants creates only the last component of `--output` itself, so a
    // fresh checkout (or the removal above) leaves it dying with `create output
    // parent directory` before a single mutant is built.
    fs::create_dir_all(&output)
        .with_context(|| format!("create Rust mutation output {}", output.display()))?;

    let output_arg = output.to_string_lossy().into_owned();
    let status = Command::new("cargo")
        .args([
            "mutants",
            "--config",
            "mutants.toml",
            "--output",
            &output_arg,
            "--baseline",
            "run",
            "--no-shuffle",
            "--no-times",
            "--colors",
            "never",
            "--annotations",
            "none",
            "--cargo-arg=--locked",
        ])
        .env("FMF_MUTATION_SOURCE_ROOT", paths::repo_root())
        .env_remove("FMF_BLESS")
        .current_dir(paths::engine_dir())
        .status()
        .context("spawn pinned cargo-mutants")?;

    let run = parse_rust_run(&cargo_mutants_report_dir(&output))?;
    write_json_atomic(
        &output.join("gate.json"),
        &RustEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            tool: expected_tool(CARGO_MUTANTS_NAME, CARGO_MUTANTS_VERSION),
            baseline_passed: true,
            examined_files: &run.examined_files,
            survivors: &run.survivors,
            timeouts: &run.timeouts,
        },
    )?;

    validate_cargo_mutants_exit(status, &run)?;
    if !run.timeouts.is_empty() {
        bail!(
            "cargo-mutants produced {} timeout(s); timeouts are never accepted (see {})",
            run.timeouts.len(),
            output.join("gate.json").display()
        );
    }

    let baseline: AcceptedBaseline<RustIdentity> = read_baseline(
        &paths::rust_mutation_baseline(),
        CARGO_MUTANTS_NAME,
        CARGO_MUTANTS_VERSION,
    )?;
    compare_exact_files(
        &baseline.examined_files,
        &run.examined_files,
        &paths::rust_mutation_baseline(),
    )?;
    compare_exact_survivors(&baseline, &run.survivors, &paths::rust_mutation_baseline())?;

    println!(
        "Rust mutation gate passed: exact survivor set matches {} ({} accepted equivalent(s)).",
        paths::rust_mutation_baseline().display(),
        run.survivors.len()
    );
    Ok(())
}

pub fn run_csharp() -> Result<()> {
    verify_stryker_manifest_pin()?;

    let repo = paths::repo_root();
    let test_dir = repo.join("app").join("FindMyFiles.Tests");
    let baseline_path = paths::csharp_mutation_baseline();
    let baseline: AcceptedBaseline<CsharpIdentity> =
        read_baseline(&baseline_path, STRYKER_NAME, STRYKER_VERSION)?;
    let reviewed_scope = read_stryker_scope(
        &test_dir.join("stryker-config.json"),
        &repo,
        &baseline.examined_files,
    )?;
    let output = paths::csharp_mutation_dir();
    let tool_manifest = repo.join(".config").join("dotnet-tools.json");
    let tool_manifest_arg = tool_manifest.to_string_lossy().into_owned();
    fsx::force_remove_dir_all(&output)
        .with_context(|| format!("remove stale C# mutation output {}", output.display()))?;

    run_required(
        Command::new("dotnet")
            .args(["tool", "restore", "--tool-manifest", &tool_manifest_arg])
            .current_dir(&test_dir),
        "restore the pinned local dotnet tools",
    )?;

    let test_results = paths::build_root()
        .join("test-results")
        .join("mutation-baseline");
    let test_results_arg = test_results.to_string_lossy().into_owned();
    run_required(
        Command::new("dotnet")
            .args([
                "test",
                "FindMyFiles.Tests.csproj",
                "--results-directory",
                &test_results_arg,
                "-p:SkipRustBuild=true",
                "-p:RestoreLockedMode=true",
                "-p:FmfTestSeams=true",
                "-p:FmfArtifactKind=ui-test",
            ])
            .current_dir(&test_dir),
        "run the unmutated C# baseline tests",
    )?;

    let output_arg = output.to_string_lossy().into_owned();
    let status = Command::new("dotnet")
        .args([
            "tool",
            "run",
            "dotnet-stryker",
            "--output",
            &output_arg,
            "--skip-version-check",
            "--break-on-initial-test-failure",
        ])
        .env("RestoreLockedMode", "true")
        .env("SkipRustBuild", "true")
        .envs(CSHARP_TEST_PROFILE)
        .current_dir(&test_dir)
        .status()
        .context("spawn pinned Stryker.NET")?;

    let report = find_unique_file(&output, "mutation-report.json")?;
    let run = parse_csharp_run(&report, &repo, &reviewed_scope)?;
    write_json_atomic(
        &output.join("gate.json"),
        &CsharpEvidence {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            tool: expected_tool(STRYKER_NAME, STRYKER_VERSION),
            baseline_passed: true,
            process_exit_code: status.code(),
            examined_files: &run.examined_files,
            survivors: &run.survivors,
            no_coverage: &run.no_coverage,
            timeouts: &run.timeouts,
            ignored: &run.ignored,
            redundant_ignored: &run.redundant_ignored,
            outside_scope: &run.outside_scope,
            outside_scope_violations: &run.outside_scope_violations,
        },
    )?;

    validate_stryker_exit(status)?;
    validate_csharp_conclusive(&run, &output.join("gate.json"))?;

    compare_exact_files(
        &baseline.examined_files,
        &run.examined_files,
        &baseline_path,
    )?;
    compare_exact_survivors(&baseline, &run.survivors, &baseline_path)?;

    println!(
        "C# mutation gate passed: exact survivor set matches {} ({} accepted equivalent(s)).",
        baseline_path.display(),
        run.survivors.len()
    );
    Ok(())
}

/// Load the canonical C# mutation scope and reviewed survivor identities.
///
/// The trusted CI controller uses this instead of accepting mutation policy
/// from the target checkout. `read_baseline` validates the tool pin, exact
/// ordering, identities, and rationales; `read_stryker_scope` additionally
/// proves that every exact mutate entry resolves to the same file inventory.
pub fn read_csharp_reviewed_policy(repo: &Path) -> Result<CsharpReviewedPolicy> {
    let test_dir = repo.join("app").join("FindMyFiles.Tests");
    let baseline_path = test_dir.join("mutation-baseline.json");
    let baseline: AcceptedBaseline<CsharpIdentity> =
        read_baseline(&baseline_path, STRYKER_NAME, STRYKER_VERSION)?;
    let scope = read_stryker_scope(
        &test_dir.join("stryker-config.json"),
        repo,
        &baseline.examined_files,
    )?;
    for (index, accepted) in baseline.accepted_equivalents.iter().enumerate() {
        if !scope.contains(&accepted.identity.path) {
            bail!(
                "{} accepted_equivalents[{index}] is outside the reviewed C# source inventory",
                baseline_path.display()
            );
        }
    }
    Ok(CsharpReviewedPolicy {
        examined_files: baseline.examined_files,
        accepted_equivalents: baseline
            .accepted_equivalents
            .into_iter()
            .map(|accepted| accepted.identity)
            .collect(),
    })
}

fn verify_cargo_mutants_version() -> Result<()> {
    let output = Command::new("cargo")
        .args(["mutants", "--version"])
        .current_dir(paths::engine_dir())
        .stderr(Stdio::inherit())
        .output()
        .context("query cargo-mutants version")?;
    if !output.status.success() {
        bail!("`cargo mutants --version` failed with {}", output.status);
    }
    let actual = std::str::from_utf8(&output.stdout)
        .context("cargo-mutants version output is not UTF-8")?
        .trim();
    let expected = format!("{CARGO_MUTANTS_NAME} {CARGO_MUTANTS_VERSION}");
    if actual != expected {
        bail!("cargo-mutants pin mismatch: expected `{expected}`, got `{actual}`");
    }
    Ok(())
}

fn verify_stryker_manifest_pin() -> Result<()> {
    let manifest = paths::repo_root().join(".config").join("dotnet-tools.json");
    let value: Value = read_json(&manifest)?;
    let tool = value
        .pointer("/tools/dotnet-stryker")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{} has no dotnet-stryker tool entry", manifest.display()))?;
    let version = tool
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{} has no dotnet-stryker version", manifest.display()))?;
    let roll_forward = tool
        .get("rollForward")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!(
                "{} has no dotnet-stryker rollForward flag",
                manifest.display()
            )
        })?;
    let commands = tool
        .get("commands")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{} has no dotnet-stryker commands", manifest.display()))?;
    if version != STRYKER_VERSION
        || roll_forward
        || commands.len() != 1
        || commands[0].as_str() != Some("dotnet-stryker")
    {
        bail!(
            "{} must pin only dotnet-stryker {STRYKER_VERSION} with rollForward=false",
            manifest.display()
        );
    }
    Ok(())
}

fn read_stryker_scope(
    config_path: &Path,
    repo_root: &Path,
    expected_files: &[String],
) -> Result<BTreeSet<String>> {
    let config: Value = read_json(config_path)?;
    let root = config
        .get("stryker-config")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{} has no stryker-config object", config_path.display()))?;
    let project = root
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{} has no project string", config_path.display()))?;
    if project != "FindMyFiles.csproj" {
        bail!(
            "{} project must remain exactly `FindMyFiles.csproj`, got `{project}`",
            config_path.display()
        );
    }
    let patterns = root
        .get("mutate")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{} has no mutate array", config_path.display()))?;
    if patterns.is_empty() {
        bail!("{} mutate scope must not be empty", config_path.display());
    }

    let project_root = repo_root.join("app").join("FindMyFiles");
    let mut scope = BTreeSet::new();
    for (index, value) in patterns.iter().enumerate() {
        let pattern = value
            .as_str()
            .ok_or_else(|| anyhow!("{} mutate[{index}] is not a string", config_path.display()))?;
        let relative = pattern.strip_prefix("**/").ok_or_else(|| {
            anyhow!(
                "{} mutate[{index}] must be an exact project-relative path prefixed by `**/`",
                config_path.display()
            )
        })?;
        if relative.is_empty()
            || relative
                .chars()
                .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}' | '!'))
        {
            bail!(
                "{} mutate[{index}] contains an unreviewable wildcard or exclusion: `{pattern}`",
                config_path.display()
            );
        }
        let canonical = canonical_repo_path(repo_root, &project_root, relative)
            .with_context(|| format!("canonicalize {} mutate[{index}]", config_path.display()))?;
        if !scope.insert(canonical.clone()) {
            bail!(
                "{} has duplicate canonical mutate target `{canonical}`",
                config_path.display()
            );
        }
        let source = repo_root.join(canonical.replace('/', std::path::MAIN_SEPARATOR_STR));
        if !source.is_file() {
            bail!(
                "{} mutate target does not exist as a regular file: {}",
                config_path.display(),
                source.display()
            );
        }
    }

    let actual: Vec<String> = scope.iter().cloned().collect();
    compare_exact_files(expected_files, &actual, config_path)?;
    Ok(scope)
}

fn run_required(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to {description}"))?;
    if !status.success() {
        bail!("{description} failed with {status}");
    }
    Ok(())
}

fn validate_cargo_mutants_exit(status: ExitStatus, run: &RustRun) -> Result<()> {
    let expected = if run.timeouts.is_empty() {
        if run.survivors.is_empty() {
            0
        } else {
            2
        }
    } else {
        3
    };
    if status.code() != Some(expected) {
        bail!(
            "cargo-mutants exit/report mismatch: exit={:?}, expected={expected} for {} survivor(s) and {} timeout(s)",
            status.code(),
            run.survivors.len(),
            run.timeouts.len()
        );
    }
    Ok(())
}

fn validate_stryker_exit(status: ExitStatus) -> Result<()> {
    if status.code() != Some(0) {
        bail!(
            "Stryker.NET exit/report mismatch: expected exit 0 after a complete run with break threshold 0, got {:?}",
            status.code()
        );
    }
    Ok(())
}

fn validate_csharp_conclusive(run: &CsharpRun, evidence: &Path) -> Result<()> {
    if !run.timeouts.is_empty() {
        bail!(
            "Stryker.NET produced {} timeout(s); timeouts are never accepted (see {})",
            run.timeouts.len(),
            evidence.display()
        );
    }
    if !run.no_coverage.is_empty() {
        bail!(
            "Stryker.NET produced {} no-coverage mutant(s); no-coverage is never accepted (see {})",
            run.no_coverage.len(),
            evidence.display()
        );
    }
    if !run.ignored.is_empty() {
        bail!(
            "Stryker.NET produced {} non-redundant ignored mutant(s) in the reviewed scope; ignored mutations are untested and never accepted (see {})",
            run.ignored.len(),
            evidence.display()
        );
    }
    if !run.outside_scope_violations.is_empty() {
        bail!(
            "Stryker.NET produced {} non-excluded mutant result(s) outside the reviewed scope; the mutate inventory did not hold (see {})",
            run.outside_scope_violations.len(),
            evidence.display()
        );
    }
    Ok(())
}

fn parse_rust_run(output: &Path) -> Result<RustRun> {
    let outcomes_path = output.join("outcomes.json");
    let outcomes: Value = read_json(&outcomes_path)?;
    let version = outcomes
        .get("cargo_mutants_version")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{} has no cargo_mutants_version", outcomes_path.display()))?;
    if version != CARGO_MUTANTS_VERSION {
        bail!(
            "{} was produced by cargo-mutants {version}, expected {CARGO_MUTANTS_VERSION}",
            outcomes_path.display()
        );
    }
    let entries = outcomes
        .get("outcomes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{} has no outcomes array", outcomes_path.display()))?;

    let mut baseline_count = 0_usize;
    let mut by_status: BTreeMap<&str, BTreeSet<RustIdentity>> = BTreeMap::new();
    let mut all_outcomes = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let object = entry.as_object().ok_or_else(|| {
            anyhow!(
                "{} outcome #{index} is not an object",
                outcomes_path.display()
            )
        })?;
        let summary = object
            .get("summary")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!(
                    "{} outcome #{index} has no summary",
                    outcomes_path.display()
                )
            })?;
        let scenario = object.get("scenario").ok_or_else(|| {
            anyhow!(
                "{} outcome #{index} has no scenario",
                outcomes_path.display()
            )
        })?;

        if scenario.as_str() == Some("Baseline") {
            baseline_count += 1;
            if summary != "Success" {
                bail!(
                    "{} records an unsuccessful unmutated baseline: {summary}",
                    outcomes_path.display()
                );
            }
            continue;
        }

        let mutant = scenario
            .get("Mutant")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                anyhow!(
                    "{} outcome #{index} has an unknown scenario",
                    outcomes_path.display()
                )
            })?;
        let name = mutant.get("name").and_then(Value::as_str).ok_or_else(|| {
            anyhow!(
                "{} mutant outcome #{index} has no name",
                outcomes_path.display()
            )
        })?;
        let identity = parse_rust_identity(name)?;
        if !all_outcomes.insert(identity.clone()) {
            bail!(
                "{} contains duplicate mutant identity `{name}`",
                outcomes_path.display()
            );
        }
        match summary {
            "CaughtMutant" | "MissedMutant" | "Timeout" | "Unviable" => {
                by_status.entry(summary).or_default().insert(identity);
            }
            other => bail!(
                "{} mutant `{name}` has non-terminal/unknown summary `{other}`",
                outcomes_path.display()
            ),
        }
    }
    if baseline_count != 1 {
        bail!(
            "{} must contain exactly one successful unmutated baseline, found {baseline_count}",
            outcomes_path.display()
        );
    }
    if all_outcomes.is_empty() {
        bail!(
            "{} contains no tested mutants; the curated scope is unexpectedly empty",
            outcomes_path.display()
        );
    }

    let generated = parse_generated_mutants(&output.join("mutants.json"))?;
    if generated != all_outcomes {
        bail!(
            "{} and {} disagree on the exact generated mutant set",
            output.join("mutants.json").display(),
            outcomes_path.display()
        );
    }
    let examined_files: Vec<String> = generated
        .iter()
        .map(|identity| identity.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    for (file, status) in [
        ("caught.txt", "CaughtMutant"),
        ("missed.txt", "MissedMutant"),
        ("timeout.txt", "Timeout"),
        ("unviable.txt", "Unviable"),
    ] {
        let from_text = parse_rust_identity_file(&output.join(file))?;
        let from_json = by_status.get(status).cloned().unwrap_or_default();
        if from_text != from_json {
            bail!(
                "{} disagrees with outcomes.json for {status}",
                output.join(file).display()
            );
        }
    }

    Ok(RustRun {
        examined_files,
        survivors: by_status
            .remove("MissedMutant")
            .unwrap_or_default()
            .into_iter()
            .collect(),
        timeouts: by_status
            .remove("Timeout")
            .unwrap_or_default()
            .into_iter()
            .collect(),
    })
}

fn parse_generated_mutants(path: &Path) -> Result<BTreeSet<RustIdentity>> {
    let value: Value = read_json(path)?;
    let mutants = value
        .as_array()
        .or_else(|| value.get("mutants").and_then(Value::as_array))
        .ok_or_else(|| anyhow!("{} has no mutant array", path.display()))?;
    let mut identities = BTreeSet::new();
    for (index, mutant) in mutants.iter().enumerate() {
        let name = mutant
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{} mutant #{index} has no name", path.display()))?;
        let identity = parse_rust_identity(name)?;
        if !identities.insert(identity) {
            bail!("{} contains duplicate mutant `{name}`", path.display());
        }
    }
    if identities.is_empty() {
        bail!("{} contains no generated mutants", path.display());
    }
    Ok(identities)
}

fn parse_rust_identity_file(path: &Path) -> Result<BTreeSet<RustIdentity>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read required cargo-mutants report {}", path.display()))?;
    let mut identities = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            bail!("{} contains a blank line at {}", path.display(), index + 1);
        }
        let identity = parse_rust_identity(line)
            .with_context(|| format!("parse {} line {}", path.display(), index + 1))?;
        if !identities.insert(identity) {
            bail!("{} contains a duplicate identity", path.display());
        }
    }
    Ok(identities)
}

fn parse_rust_identity(name: &str) -> Result<RustIdentity> {
    if name.trim() != name || name.chars().any(char::is_control) {
        bail!("cargo-mutants identity has surrounding whitespace or control characters");
    }
    let captures = RUST_IDENTITY
        .captures(name)
        .ok_or_else(|| anyhow!("malformed cargo-mutants identity `{name}`"))?;
    let path = captures
        .name("path")
        .ok_or_else(|| anyhow!("cargo-mutants identity has no path"))?
        .as_str();
    let line = captures
        .name("line")
        .ok_or_else(|| anyhow!("cargo-mutants identity has no line"))?
        .as_str()
        .parse()
        .context("cargo-mutants line number is invalid")?;
    let column = captures
        .name("column")
        .map(|value| value.as_str().parse())
        .transpose()
        .context("cargo-mutants column number is invalid")?;
    if column == Some(0) {
        bail!("cargo-mutants columns are one-based when present");
    }
    let mutation = captures
        .name("mutation")
        .ok_or_else(|| anyhow!("cargo-mutants identity has no mutation"))?
        .as_str();
    if mutation.trim() != mutation || mutation.is_empty() {
        bail!("cargo-mutants mutation description is empty or not canonical");
    }
    Ok(RustIdentity {
        path: canonical_repo_path(&paths::repo_root(), &paths::engine_dir(), path)?,
        line,
        column,
        mutation: mutation.to_owned(),
    })
}

fn parse_csharp_run(
    report_path: &Path,
    repo_root: &Path,
    reviewed_scope: &BTreeSet<String>,
) -> Result<CsharpRun> {
    if reviewed_scope.is_empty() {
        bail!("reviewed Stryker scope must not be empty");
    }
    let report: Value = read_json(report_path)?;
    let root = report
        .as_object()
        .ok_or_else(|| anyhow!("{} root is not an object", report_path.display()))?;
    let schema = root
        .get("schemaVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{} has no schemaVersion", report_path.display()))?;
    if schema != "2" {
        bail!(
            "{} schemaVersion is `{schema}`, expected exact Stryker 4.16 schema `2`",
            report_path.display()
        );
    }
    let project_root = root
        .get("projectRoot")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{} has no projectRoot", report_path.display()))?;
    let files = root
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{} has no files object", report_path.display()))?;
    if files.is_empty() {
        bail!("{} contains no mutated files", report_path.display());
    }

    let mut all = BTreeSet::new();
    let mut survivors = BTreeSet::new();
    let mut no_coverage = BTreeSet::new();
    let mut timeouts = BTreeSet::new();
    let mut ignored = BTreeSet::new();
    let mut redundant_ignored = BTreeSet::new();
    let mut outside_scope = OutsideScopeSummary::default();
    let mut outside_scope_violations = BTreeSet::new();
    let mut report_files = BTreeSet::new();
    let mut reviewed_files = BTreeSet::new();
    let mut reviewed_mutant_count = 0_usize;
    for (file, file_report) in files {
        let canonical_path = canonical_repo_path(repo_root, Path::new(project_root), file)
            .with_context(|| {
                format!(
                    "canonicalize Stryker report path `{file}` from project root `{project_root}`"
                )
            })?;
        if !report_files.insert(canonical_path.clone()) {
            bail!(
                "{} contains duplicate canonical source-file identity `{canonical_path}`",
                report_path.display()
            );
        }
        let in_scope = reviewed_scope.contains(&canonical_path);
        if in_scope {
            reviewed_files.insert(canonical_path.clone());
        } else {
            outside_scope.file_count += 1;
        }
        let mutants = file_report
            .get("mutants")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                anyhow!(
                    "{} file `{file}` has no mutants array",
                    report_path.display()
                )
            })?;
        if in_scope && mutants.is_empty() {
            bail!(
                "{} reviewed file `{canonical_path}` contains no mutants",
                report_path.display()
            );
        }
        for (index, mutant) in mutants.iter().enumerate() {
            if in_scope {
                reviewed_mutant_count += 1;
            } else {
                outside_scope.mutant_count += 1;
            }
            let identity = parse_csharp_identity(&canonical_path, mutant).with_context(|| {
                format!(
                    "parse {} file `{file}` mutant #{index}",
                    report_path.display()
                )
            })?;
            if !all.insert(identity.clone()) {
                bail!(
                    "{} contains duplicate canonical mutant identity in `{file}`",
                    report_path.display()
                );
            }
            let status = mutant
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("mutant has no status"))?;
            if !matches!(
                status,
                "Killed" | "CompileError" | "Survived" | "NoCoverage" | "Timeout" | "Ignored"
            ) {
                bail!("mutant has non-terminal/unknown status `{status}`");
            }
            let status_reason = optional_clean_string(mutant.get("statusReason"), "statusReason")?;
            let status_identity = CsharpStatusIdentity {
                identity: identity.clone(),
                status: status.to_owned(),
                status_reason: status_reason.clone(),
            };

            if !in_scope {
                *outside_scope
                    .status_counts
                    .entry(status.to_owned())
                    .or_default() += 1;
                if !outside_scope_is_unexecuted(status, status_reason.as_deref()) {
                    outside_scope_violations.insert(status_identity);
                }
                continue;
            }

            match status {
                "Killed" | "CompileError" => {}
                "Survived" => {
                    survivors.insert(identity);
                }
                "NoCoverage" => {
                    no_coverage.insert(identity);
                }
                "Timeout" => {
                    timeouts.insert(identity);
                }
                "Ignored"
                    if identity.mutator == "Block removal mutation"
                        && status_reason.as_deref()
                            == Some("Removed by block already covered filter") =>
                {
                    redundant_ignored.insert(status_identity);
                }
                "Ignored" => {
                    ignored.insert(identity);
                }
                _ => unreachable!("terminal statuses were validated above"),
            }
        }
    }
    let missing: Vec<&String> = reviewed_scope.difference(&reviewed_files).collect();
    if !missing.is_empty() {
        bail!(
            "{} omits {} reviewed source file(s): {}",
            report_path.display(),
            missing.len(),
            serde_json::to_string_pretty(&missing)?
        );
    }
    if reviewed_mutant_count == 0 {
        bail!(
            "{} contains no mutants; the curated Stryker scope is unexpectedly empty",
            report_path.display()
        );
    }
    Ok(CsharpRun {
        examined_files: reviewed_scope.iter().cloned().collect(),
        survivors: survivors.into_iter().collect(),
        no_coverage: no_coverage.into_iter().collect(),
        timeouts: timeouts.into_iter().collect(),
        ignored: ignored.into_iter().collect(),
        redundant_ignored: redundant_ignored.into_iter().collect(),
        outside_scope,
        outside_scope_violations: outside_scope_violations.into_iter().collect(),
    })
}

/// Stryker's reasons for an `Ignored` mutant that was dropped before any test
/// could run it. `Removed by mutate filter` is how the reviewed `mutate`
/// inventory is enforced in the first place.
const UNEXECUTED_IGNORE_REASONS: [&str; 2] = [
    "Removed by mutate filter",
    "Removed by exclude from code coverage filter",
];

/// Whether an out-of-scope mutant result proves the mutant was never executed.
///
/// Stryker does not narrow *mutation* to the `mutate` inventory. It mutates the
/// whole project, compiles that, and only then filters: with a single file in
/// scope this app still produced `6308 mutants created`, of which `5001 mutants
/// got status Ignored. Reason: Removed by mutate filter` and `3 total mutants
/// will be tested`. Files outside the reviewed inventory therefore appear in the
/// report by construction, so the question the gate must answer is not "was
/// anything mutated outside the scope" (always yes) but "was anything *executed*
/// outside it".
///
/// Two outcomes answer no:
/// * `Ignored` for a filter reason — dropped before testing.
/// * `CompileError` — the mutated tree did not build, so Safe Mode rolled every
///   mutant in the enclosing method back out of the assembly (`Safe Mode! Stryker
///   will remove all mutations in <method>`). Compilation precedes filtering,
///   which is why these carry `CompileError` rather than the mutate-filter
///   reason. In-scope `CompileError` is accepted for exactly the same reason: a
///   mutant that is not in the binary cannot change a verdict.
///
/// Everything else — `Killed`, `Survived`, `NoCoverage`, `Timeout` — means
/// Stryker activated a mutant the reviewed inventory does not cover. That is the
/// mutate scope failing to hold, and the gate must not absorb it.
fn outside_scope_is_unexecuted(status: &str, status_reason: Option<&str>) -> bool {
    match status {
        "CompileError" => true,
        "Ignored" => {
            status_reason.is_some_and(|reason| UNEXECUTED_IGNORE_REASONS.contains(&reason))
        }
        _ => false,
    }
}

fn parse_csharp_identity(path: &str, mutant: &Value) -> Result<CsharpIdentity> {
    let object = mutant
        .as_object()
        .ok_or_else(|| anyhow!("mutant is not an object"))?;
    let mutator = required_clean_string(object.get("mutatorName"), "mutatorName", false)?;
    let replacement = required_clean_string(object.get("replacement"), "replacement", true)?;
    let location = object
        .get("location")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("mutant has no location"))?;
    let start = location
        .get("start")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("mutant has no location.start"))?;
    let end = location
        .get("end")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("mutant has no location.end"))?;
    let start_line = required_u64(start.get("line"), "location.start.line", 1)?;
    let start_column = required_u64(start.get("column"), "location.start.column", 0)?;
    let end_line = required_u64(end.get("line"), "location.end.line", 1)?;
    let end_column = required_u64(end.get("column"), "location.end.column", 0)?;
    if (end_line, end_column) < (start_line, start_column) {
        bail!("mutant location ends before it starts");
    }
    Ok(CsharpIdentity {
        path: path.to_owned(),
        start_line,
        start_column,
        end_line,
        end_column,
        mutator,
        replacement,
    })
}

fn required_clean_string(value: Option<&Value>, field: &str, allow_empty: bool) -> Result<String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("mutant has no {field} string"))?;
    if value.chars().any(|character| character == '\0') || (!allow_empty && value.trim().is_empty())
    {
        bail!("mutant {field} is invalid");
    }
    Ok(value.to_owned())
}

fn optional_clean_string(value: Option<&Value>, field: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| anyhow!("mutant {field} is not a string"))?;
    if value.chars().any(|character| character == '\0') {
        bail!("mutant {field} contains NUL");
    }
    Ok(Some(value.to_owned()))
}

fn required_u64(value: Option<&Value>, field: &str, minimum: u64) -> Result<u64> {
    let value = value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("mutant has no non-negative integer {field}"))?;
    if value < minimum {
        bail!("mutant {field} is below {minimum}");
    }
    Ok(value)
}

fn find_unique_file(root: &Path, file_name: &str) -> Result<PathBuf> {
    if !root.is_dir() {
        bail!(
            "required mutation output directory is missing: {}",
            root.display()
        );
    }
    let mut pending = vec![root.to_path_buf()];
    let mut matches = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read mutation output {}", directory.display()))?
        {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || fsx::is_reparse_point(&metadata) {
                bail!(
                    "mutation output contains a link/reparse point: {}",
                    entry.path().display()
                );
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name() == file_name {
                matches.push(entry.path());
            }
        }
    }
    if matches.len() != 1 {
        bail!(
            "expected exactly one `{file_name}` below {}, found {}",
            root.display(),
            matches.len()
        );
    }
    matches
        .pop()
        .ok_or_else(|| anyhow!("unique report disappeared"))
}

fn read_baseline<T>(
    path: &Path,
    expected_name: &str,
    expected_version: &str,
) -> Result<AcceptedBaseline<T>>
where
    T: Clone + DeserializeOwned + Ord + Serialize + ValidateIdentity,
{
    let baseline: AcceptedBaseline<T> = read_json(path)?;
    if baseline.schema_version != BASELINE_SCHEMA_VERSION {
        bail!(
            "{} schema_version is {}, expected {}",
            path.display(),
            baseline.schema_version,
            BASELINE_SCHEMA_VERSION
        );
    }
    let expected_tool = expected_tool(expected_name, expected_version);
    if baseline.tool != expected_tool {
        bail!(
            "{} tool pin is {:?}, expected {:?}",
            path.display(),
            baseline.tool,
            expected_tool
        );
    }

    let mut previous: Option<&T> = None;
    for (index, accepted) in baseline.accepted_equivalents.iter().enumerate() {
        accepted
            .identity
            .validate()
            .with_context(|| format!("{} accepted_equivalents[{index}]", path.display()))?;
        if accepted.rationale.trim() != accepted.rationale
            || accepted.rationale.len() < 12
            || accepted.rationale.chars().any(char::is_control)
        {
            bail!(
                "{} accepted_equivalents[{index}] needs a canonical, specific rationale",
                path.display()
            );
        }
        if previous.is_some_and(|value| value >= &accepted.identity) {
            bail!(
                "{} accepted_equivalents must be strictly sorted with no duplicates",
                path.display()
            );
        }
        previous = Some(&accepted.identity);
    }
    validate_examined_files(path, &baseline.examined_files)?;
    Ok(baseline)
}

trait ValidateIdentity {
    fn validate(&self) -> Result<()>;
}

impl ValidateIdentity for RustIdentity {
    fn validate(&self) -> Result<()> {
        validate_relative_path(&self.path)?;
        if self.line == 0 {
            bail!("line must be positive");
        }
        if self.column == Some(0) {
            bail!("cargo-mutants columns are one-based when present");
        }
        if self.mutation.trim() != self.mutation
            || self.mutation.is_empty()
            || self.mutation.chars().any(char::is_control)
        {
            bail!("mutation description is not canonical");
        }
        Ok(())
    }
}

impl ValidateIdentity for CsharpIdentity {
    fn validate(&self) -> Result<()> {
        validate_relative_path(&self.path)?;
        if self.start_line == 0
            || self.end_line == 0
            || (self.end_line, self.end_column) < (self.start_line, self.start_column)
        {
            bail!("source span is invalid");
        }
        if self.mutator.trim().is_empty() || self.mutator.chars().any(char::is_control) {
            bail!("mutator is invalid");
        }
        if self.replacement.chars().any(|character| character == '\0') {
            bail!("replacement contains NUL");
        }
        Ok(())
    }
}

fn compare_exact_survivors<T>(
    baseline: &AcceptedBaseline<T>,
    actual: &[T],
    baseline_path: &Path,
) -> Result<()>
where
    T: Clone + Ord + Serialize,
{
    let expected: Vec<T> = baseline
        .accepted_equivalents
        .iter()
        .map(|accepted| accepted.identity.clone())
        .collect();
    if expected == actual {
        return Ok(());
    }

    let expected_set: BTreeSet<&T> = expected.iter().collect();
    let actual_set: BTreeSet<&T> = actual.iter().collect();
    let new: Vec<&&T> = actual_set.difference(&expected_set).collect();
    let missing: Vec<&&T> = expected_set.difference(&actual_set).collect();
    bail!(
        "mutation survivor baseline drift in {}: {} new survivor(s), {} missing accepted survivor(s)\nnew={}\nmissing={}",
        baseline_path.display(),
        new.len(),
        missing.len(),
        serde_json::to_string_pretty(&new)?,
        serde_json::to_string_pretty(&missing)?
    )
}

fn validate_examined_files(path: &Path, examined_files: &[String]) -> Result<()> {
    if examined_files.is_empty() {
        bail!("{} examined_files must not be empty", path.display());
    }
    let mut previous: Option<&str> = None;
    for (index, file) in examined_files.iter().enumerate() {
        validate_relative_path(file)
            .with_context(|| format!("{} examined_files[{index}]", path.display()))?;
        if previous.is_some_and(|value| value >= file.as_str()) {
            bail!(
                "{} examined_files must be strictly sorted with no duplicates",
                path.display()
            );
        }
        previous = Some(file);
    }
    Ok(())
}

fn compare_exact_files(expected: &[String], actual: &[String], baseline_path: &Path) -> Result<()> {
    if expected == actual {
        return Ok(());
    }
    let expected_set: BTreeSet<&String> = expected.iter().collect();
    let actual_set: BTreeSet<&String> = actual.iter().collect();
    let added: Vec<&&String> = actual_set.difference(&expected_set).collect();
    let missing: Vec<&&String> = expected_set.difference(&actual_set).collect();
    bail!(
        "mutation examined-file inventory drift in {}: {} added, {} missing\nadded={}\nmissing={}",
        baseline_path.display(),
        added.len(),
        missing.len(),
        serde_json::to_string_pretty(&added)?,
        serde_json::to_string_pretty(&missing)?
    )
}

fn expected_tool(name: &str, version: &str) -> ToolPin {
    ToolPin {
        name: name.to_owned(),
        version: version.to_owned(),
    }
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read required JSON {}", path.display()))?;
    let value: StrictJsonValue = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse strict JSON {}", path.display()))?;
    serde_json::from_value(value.0)
        .with_context(|| format!("decode strict JSON {}", path.display()))
}

struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value with no duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| E::custom("JSON number is not finite"))?;
        Ok(StrictJsonValue(Value::Number(number)))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictJsonValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key `{key}`"
                )));
            }
            let value = object.next_value::<StrictJsonValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(values)))
    }
}

pub fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fsx::write_file_atomic(path, &bytes)
        .with_context(|| format!("write canonical mutation evidence {}", path.display()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathRoot {
    Unix,
    Drive(char),
    Unc(String, String),
    Relative,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LexicalPath {
    root: PathRoot,
    segments: Vec<String>,
}

fn canonical_repo_path(repo_root: &Path, project_root: &Path, file: &str) -> Result<String> {
    let repo = parse_path(&repo_root.to_string_lossy())?;
    if repo.root == PathRoot::Relative {
        bail!("repository root is not absolute: {}", repo_root.display());
    }

    let project = parse_path(&project_root.to_string_lossy())?;
    let project = if project.root == PathRoot::Relative {
        join_path(&repo, &project)?
    } else {
        project
    };
    let file = parse_path(file)?;
    let absolute = if file.root == PathRoot::Relative {
        join_path(&project, &file)?
    } else {
        file
    };
    let case_insensitive = matches!(repo.root, PathRoot::Drive(_) | PathRoot::Unc(_, _));
    if !roots_equal(&repo.root, &absolute.root, case_insensitive)
        || absolute.segments.len() <= repo.segments.len()
        || !repo
            .segments
            .iter()
            .zip(&absolute.segments)
            .all(|(left, right)| component_equal(left, right, case_insensitive))
    {
        bail!(
            "reported source path is outside the repository: `{}`",
            absolute.render()
        );
    }
    let relative = absolute.segments[repo.segments.len()..].join("/");
    validate_relative_path(&relative)?;
    Ok(relative)
}

fn parse_path(input: &str) -> Result<LexicalPath> {
    if input.is_empty() || input.chars().any(char::is_control) {
        bail!("path is empty or contains control characters");
    }
    let normalized = input.replace('\\', "/");
    if normalized.starts_with("//?/") || normalized.starts_with("//./") {
        bail!("device paths are not valid source identities");
    }

    let (root, rest) = if let Some(rest) = normalized.strip_prefix("//") {
        let mut parts = rest.split('/');
        let server = parts
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("UNC path has no server"))?;
        let share = parts
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("UNC path has no share"))?;
        (
            PathRoot::Unc(server.to_owned(), share.to_owned()),
            parts.collect::<Vec<_>>().join("/"),
        )
    } else if normalized.len() >= 3
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'/'
        && normalized.as_bytes()[0].is_ascii_alphabetic()
    {
        (
            PathRoot::Drive(char::from(normalized.as_bytes()[0]).to_ascii_uppercase()),
            normalized[3..].to_owned(),
        )
    } else if let Some(rest) = normalized.strip_prefix('/') {
        (PathRoot::Unix, rest.to_owned())
    } else {
        (PathRoot::Relative, normalized)
    };

    let mut segments = Vec::new();
    for segment in rest.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|value| value != "..") {
                    segments.pop();
                } else if root == PathRoot::Relative {
                    segments.push("..".to_owned());
                } else {
                    bail!("path escapes its lexical root");
                }
            }
            value if value.contains(':') => bail!("path segment contains a colon"),
            value => segments.push(value.to_owned()),
        }
    }
    Ok(LexicalPath { root, segments })
}

fn join_path(base: &LexicalPath, relative: &LexicalPath) -> Result<LexicalPath> {
    if relative.root != PathRoot::Relative {
        bail!("join operand is not relative");
    }
    let mut joined = base.clone();
    for segment in &relative.segments {
        if segment == ".." {
            if joined.segments.pop().is_none() {
                bail!("path escapes its lexical root");
            }
        } else {
            joined.segments.push(segment.clone());
        }
    }
    Ok(joined)
}

fn roots_equal(left: &PathRoot, right: &PathRoot, case_insensitive: bool) -> bool {
    match (left, right) {
        (PathRoot::Unix, PathRoot::Unix) | (PathRoot::Relative, PathRoot::Relative) => true,
        (PathRoot::Drive(left), PathRoot::Drive(right)) => left.eq_ignore_ascii_case(right),
        (PathRoot::Unc(ls, lh), PathRoot::Unc(rs, rh)) => {
            component_equal(ls, rs, case_insensitive) && component_equal(lh, rh, case_insensitive)
        }
        _ => false,
    }
}

fn component_equal(left: &str, right: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

impl LexicalPath {
    fn render(&self) -> String {
        let prefix = match &self.root {
            PathRoot::Unix => "/".to_owned(),
            PathRoot::Drive(drive) => format!("{drive}:/"),
            PathRoot::Unc(server, share) => format!("//{server}/{share}/"),
            PathRoot::Relative => String::new(),
        };
        format!("{prefix}{}", self.segments.join("/"))
    }
}

fn validate_relative_path(path: &str) -> Result<()> {
    let parsed = parse_path(path)?;
    if parsed.root != PathRoot::Relative
        || parsed.segments.is_empty()
        || parsed.render() != path
        || path.contains('\\')
    {
        bail!("path is not a canonical repository-relative path: `{path}`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_identity(path: &str, line: u64, mutation: &str) -> RustIdentity {
        RustIdentity {
            path: path.to_owned(),
            line,
            column: Some(7),
            mutation: mutation.to_owned(),
        }
    }

    fn csharp_identity(path: &str, status_seed: u64) -> CsharpIdentity {
        CsharpIdentity {
            path: path.to_owned(),
            start_line: status_seed,
            start_column: 2,
            end_line: status_seed,
            end_column: 5,
            mutator: "Boolean".to_owned(),
            replacement: "false".to_owned(),
        }
    }

    fn csharp_scope(path: &str) -> BTreeSet<String> {
        BTreeSet::from([path.to_owned()])
    }

    #[test]
    fn windows_paths_are_canonicalized_relative_to_the_repo() {
        let actual = canonical_repo_path(
            Path::new(r"C:\work\find-my-files"),
            Path::new(r"c:\WORK\find-my-files\app\FindMyFiles"),
            r".\Services\..\Engine\PipeProtocol.cs",
        )
        .expect("path should be inside the repo");
        assert_eq!(actual, "app/FindMyFiles/Engine/PipeProtocol.cs");
    }

    #[test]
    fn path_normalization_rejects_repo_escape_and_device_paths() {
        assert!(canonical_repo_path(
            Path::new(r"C:\work\repo"),
            Path::new(r"C:\work\repo\app"),
            r"..\..\secret.cs",
        )
        .is_err());
        assert!(parse_path(r"\\?\C:\work\repo\file.cs").is_err());
    }

    #[test]
    fn rust_identity_parser_normalizes_slashes_and_preserves_exact_mutation() {
        let parsed = parse_rust_identity(
            r"crates\fmf-core\src\query\exec.rs:42:7: replace `left == right` with `left != right`",
        )
        .expect("identity should parse");
        assert_eq!(parsed.path, "engine/crates/fmf-core/src/query/exec.rs");
        assert_eq!(parsed.line, 42);
        assert_eq!(parsed.column, Some(7));
        assert_eq!(
            parsed.mutation,
            "replace `left == right` with `left != right`"
        );
        assert!(parse_rust_identity("crates/fmf-core/src/wtf8.rs:42:0: replace x").is_err());
    }

    /// The local gate and the CI shard runner must resolve the same directory
    /// from the same `--output` argument. They disagreed once (`<D>` vs
    /// `<D>/mutants.out`), which made a fully completed local run die reading a
    /// report that was never going to be there.
    #[test]
    fn cargo_mutants_reports_live_one_level_below_the_output_argument() {
        assert_eq!(
            cargo_mutants_report_dir(Path::new("build/mutation/rust")),
            Path::new("build/mutation/rust").join("mutants.out")
        );
    }

    #[test]
    fn duplicate_rust_report_lines_are_rejected() {
        let base =
            std::env::temp_dir().join(format!("xtask-mutation-duplicate-{}", std::process::id()));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("missed.txt");
        let line = "crates/fmf-core/src/wtf8.rs:10:3: replace x with y\n";
        fs::write(&path, format!("{line}{line}")).expect("write fixture");
        assert!(parse_rust_identity_file(&path).is_err());
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn malformed_rust_outcome_without_baseline_fails_closed() {
        let base =
            std::env::temp_dir().join(format!("xtask-mutation-malformed-{}", std::process::id()));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        fs::write(
            base.join("outcomes.json"),
            r#"{"cargo_mutants_version":"27.1.0","outcomes":[]}"#,
        )
        .expect("write fixture");
        assert!(parse_rust_run(&base).is_err());
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn strict_json_rejects_duplicate_keys_at_every_depth() {
        let base = std::env::temp_dir().join(format!(
            "xtask-mutation-json-duplicate-{}",
            std::process::id()
        ));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("duplicate.json");
        fs::write(
            &path,
            br#"{"schemaVersion":"2","files":{"a":{"mutants":[],"mutants":[]}}}"#,
        )
        .expect("write fixture");
        assert!(read_json::<Value>(&path).is_err());
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn csharp_report_extracts_exact_terminal_problem_identities() {
        let report = serde_json::json!({
            "schemaVersion": "2",
            "projectRoot": "C:\\repo\\app\\FindMyFiles",
            "files": {
                ".\\Engine\\PipeProtocol.cs": {
                    "language": "cs",
                    "mutants": [
                        {
                            "id": "1",
                            "mutatorName": "Boolean",
                            "replacement": "false",
                            "status": "Survived",
                            "location": {
                                "start": {"line": 8, "column": 2},
                                "end": {"line": 8, "column": 6}
                            }
                        },
                        {
                            "id": "2",
                            "mutatorName": "Equality",
                            "replacement": "!=",
                            "status": "NoCoverage",
                            "location": {
                                "start": {"line": 9, "column": 1},
                                "end": {"line": 9, "column": 3}
                            }
                        },
                        {
                            "id": "3",
                            "mutatorName": "Block",
                            "replacement": "{}",
                            "status": "Timeout",
                            "location": {
                                "start": {"line": 10, "column": 0},
                                "end": {"line": 12, "column": 1}
                            }
                        }
                    ]
                }
            }
        });
        let base =
            std::env::temp_dir().join(format!("xtask-mutation-csharp-{}", std::process::id()));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("mutation-report.json");
        fs::write(
            &path,
            serde_json::to_vec(&report).expect("serialize fixture"),
        )
        .expect("write fixture");

        let parsed = parse_csharp_run(
            &path,
            Path::new(r"C:\repo"),
            &csharp_scope("app/FindMyFiles/Engine/PipeProtocol.cs"),
        )
        .expect("report should parse");
        assert_eq!(parsed.survivors.len(), 1);
        assert_eq!(parsed.no_coverage.len(), 1);
        assert_eq!(parsed.timeouts.len(), 1);
        assert_eq!(
            parsed.survivors[0].path,
            "app/FindMyFiles/Engine/PipeProtocol.cs"
        );
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn csharp_scope_allows_only_exact_redundant_blocks_and_excluded_outside_mutants() {
        let report = serde_json::json!({
            "schemaVersion": "2",
            "projectRoot": "C:\\repo\\app\\FindMyFiles",
            "files": {
                "Engine/PipeProtocol.cs": {
                    "mutants": [{
                        "mutatorName": "Block removal mutation",
                        "replacement": "{}",
                        "status": "Ignored",
                        "statusReason": "Removed by block already covered filter",
                        "location": {
                            "start": {"line": 8, "column": 2},
                            "end": {"line": 12, "column": 3}
                        }
                    }]
                },
                "App.xaml.cs": {
                    "mutants": [{
                        "mutatorName": "Boolean mutation",
                        "replacement": "false",
                        "status": "Ignored",
                        "statusReason": "Removed by exclude from code coverage filter",
                        "location": {
                            "start": {"line": 20, "column": 2},
                            "end": {"line": 20, "column": 6}
                        }
                    }]
                }
            }
        });
        let base = std::env::temp_dir().join(format!(
            "xtask-mutation-csharp-scope-{}",
            std::process::id()
        ));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("mutation-report.json");
        fs::write(
            &path,
            serde_json::to_vec(&report).expect("serialize fixture"),
        )
        .expect("write fixture");

        let parsed = parse_csharp_run(
            &path,
            Path::new(r"C:\repo"),
            &csharp_scope("app/FindMyFiles/Engine/PipeProtocol.cs"),
        )
        .expect("strictly classified report should parse");
        assert_eq!(parsed.redundant_ignored.len(), 1);
        assert!(parsed.ignored.is_empty());
        assert!(parsed.outside_scope_violations.is_empty());
        assert_eq!(parsed.outside_scope.file_count, 1);
        assert_eq!(parsed.outside_scope.mutant_count, 1);
        assert_eq!(parsed.outside_scope.status_counts["Ignored"], 1);

        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn csharp_scope_rejects_executed_results_outside_the_reviewed_inventory() {
        let violation = CsharpStatusIdentity {
            identity: csharp_identity("app/FindMyFiles/App.xaml.cs", 20),
            status: "Killed".to_owned(),
            status_reason: None,
        };
        let run = CsharpRun {
            outside_scope_violations: vec![violation],
            ..clean_for_policy_test()
        };
        assert!(validate_csharp_conclusive(&run, Path::new("gate.json")).is_err());
    }

    /// Out of scope, the only acceptable outcomes are the ones that prove the
    /// mutant never ran. A verdict there means the `mutate` inventory did not
    /// hold, which is the whole point of tracking out-of-scope results.
    #[test]
    fn outside_scope_accepts_only_unexecuted_mutant_outcomes() {
        for (status, reason) in [
            ("CompileError", Some("Mutant caused compile errors")),
            ("CompileError", None),
            ("Ignored", Some("Removed by mutate filter")),
            (
                "Ignored",
                Some("Removed by exclude from code coverage filter"),
            ),
        ] {
            assert!(
                outside_scope_is_unexecuted(status, reason),
                "an unexecuted outcome was flagged as a scope violation: {status} {reason:?}"
            );
        }
        for (status, reason) in [
            ("Killed", None),
            ("Survived", None),
            ("NoCoverage", None),
            ("Timeout", None),
            ("Ignored", None),
            ("Ignored", Some("Removed by block already covered filter")),
            ("Ignored", Some("Ignored via attribute")),
        ] {
            assert!(
                !outside_scope_is_unexecuted(status, reason),
                "an executed or unexplained outcome was accepted: {status} {reason:?}"
            );
        }
    }

    /// The exact shape the smoke run produced: 22 Safe-Mode rollbacks in
    /// `MainPage.xaml.cs` / `MainWindow.xaml.cs`, which used to bail the gate
    /// before it ever compared survivors.
    #[test]
    fn csharp_scope_tolerates_safe_mode_rollbacks_outside_the_reviewed_inventory() {
        let report = serde_json::json!({
            "schemaVersion": "2",
            "projectRoot": "C:\\repo\\app\\FindMyFiles",
            "files": {
                "Engine/PipeProtocol.cs": {
                    "mutants": [{
                        "mutatorName": "Boolean mutation",
                        "replacement": "false",
                        "status": "Killed",
                        "location": {
                            "start": {"line": 8, "column": 2},
                            "end": {"line": 8, "column": 6}
                        }
                    }]
                },
                "MainPage.xaml.cs": {
                    "mutants": [{
                        "mutatorName": "Block removal mutation",
                        "replacement": "{}",
                        "status": "CompileError",
                        "statusReason": "Mutant caused compile errors",
                        "location": {
                            "start": {"line": 240, "column": 5},
                            "end": {"line": 266, "column": 6}
                        }
                    }]
                }
            }
        });
        let base = std::env::temp_dir().join(format!(
            "xtask-mutation-csharp-rollback-{}",
            std::process::id()
        ));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("mutation-report.json");
        fs::write(
            &path,
            serde_json::to_vec(&report).expect("serialize fixture"),
        )
        .expect("write fixture");

        let parsed = parse_csharp_run(
            &path,
            Path::new(r"C:\repo"),
            &csharp_scope("app/FindMyFiles/Engine/PipeProtocol.cs"),
        )
        .expect("rolled-back out-of-scope mutants must not fail the report parse");
        assert!(parsed.outside_scope_violations.is_empty());
        assert_eq!(parsed.outside_scope.status_counts["CompileError"], 1);
        assert!(validate_csharp_conclusive(&parsed, Path::new("gate.json")).is_ok());

        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn duplicate_csharp_identity_is_rejected_even_when_ids_differ() {
        let mutant = serde_json::json!({
            "mutatorName": "Boolean",
            "replacement": "false",
            "status": "Killed",
            "location": {
                "start": {"line": 8, "column": 2},
                "end": {"line": 8, "column": 6}
            }
        });
        let report = serde_json::json!({
            "schemaVersion": "2",
            "projectRoot": "/repo/app/FindMyFiles",
            "files": {
                "Engine/PipeProtocol.cs": {
                    "mutants": [mutant.clone(), mutant]
                }
            }
        });
        let base = std::env::temp_dir().join(format!(
            "xtask-mutation-csharp-duplicate-{}",
            std::process::id()
        ));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("mutation-report.json");
        fs::write(
            &path,
            serde_json::to_vec(&report).expect("serialize fixture"),
        )
        .expect("write fixture");
        assert!(parse_csharp_run(
            &path,
            Path::new("/repo"),
            &csharp_scope("app/FindMyFiles/Engine/PipeProtocol.cs"),
        )
        .is_err());
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn csharp_report_requires_the_exact_pinned_schema() {
        let report = serde_json::json!({
            "schemaVersion": "2.0",
            "projectRoot": "/repo/app/FindMyFiles",
            "files": {}
        });
        let base = std::env::temp_dir().join(format!(
            "xtask-mutation-csharp-schema-{}",
            std::process::id()
        ));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let path = base.join("mutation-report.json");
        fs::write(
            &path,
            serde_json::to_vec(&report).expect("serialize fixture"),
        )
        .expect("write fixture");
        assert!(parse_csharp_run(
            &path,
            Path::new("/repo"),
            &csharp_scope("app/FindMyFiles/Engine/PipeProtocol.cs"),
        )
        .is_err());
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    #[test]
    fn exact_baseline_comparison_rejects_new_and_missing_survivors() {
        let accepted = rust_identity("engine/crates/fmf-core/src/wtf8.rs", 10, "replace x");
        let baseline = AcceptedBaseline {
            schema_version: BASELINE_SCHEMA_VERSION,
            tool: expected_tool(CARGO_MUTANTS_NAME, CARGO_MUTANTS_VERSION),
            examined_files: vec!["engine/crates/fmf-core/src/wtf8.rs".to_owned()],
            accepted_equivalents: vec![AcceptedEquivalent {
                identity: accepted.clone(),
                rationale: "Operands are provably disjoint.".to_owned(),
            }],
        };
        assert!(compare_exact_survivors(
            &baseline,
            std::slice::from_ref(&accepted),
            Path::new("baseline.json"),
        )
        .is_ok());
        assert!(compare_exact_survivors::<RustIdentity>(
            &baseline,
            &[],
            Path::new("baseline.json"),
        )
        .is_err());
        let new = rust_identity("engine/crates/fmf-core/src/wtf8.rs", 11, "replace y");
        assert!(
            compare_exact_survivors(&baseline, &[accepted, new], Path::new("baseline.json"),)
                .is_err()
        );
    }

    #[test]
    fn exact_examined_file_inventory_rejects_added_missing_and_reordered_files() {
        let expected = vec![
            "engine/crates/fmf-core/src/query/exec.rs".to_owned(),
            "engine/crates/fmf-core/src/wtf8.rs".to_owned(),
        ];
        assert!(compare_exact_files(&expected, &expected, Path::new("baseline.json")).is_ok());
        assert!(compare_exact_files(
            &expected,
            &["engine/crates/fmf-core/src/wtf8.rs".to_owned()],
            Path::new("baseline.json"),
        )
        .is_err());
        let reversed = expected.iter().rev().cloned().collect::<Vec<_>>();
        assert!(compare_exact_files(&expected, &reversed, Path::new("baseline.json")).is_err());
    }

    #[test]
    fn csharp_timeout_no_coverage_and_ignored_are_never_conclusive() {
        let identity = csharp_identity("app/FindMyFiles/Engine/PipeProtocol.cs", 3);
        let clean = CsharpRun {
            examined_files: vec!["app/FindMyFiles/Engine/PipeProtocol.cs".to_owned()],
            survivors: Vec::new(),
            no_coverage: Vec::new(),
            timeouts: Vec::new(),
            ignored: Vec::new(),
            redundant_ignored: Vec::new(),
            outside_scope: OutsideScopeSummary::default(),
            outside_scope_violations: Vec::new(),
        };
        assert!(validate_csharp_conclusive(&clean, Path::new("gate.json")).is_ok());

        for run in [
            CsharpRun {
                no_coverage: vec![identity.clone()],
                ..clean_for_policy_test()
            },
            CsharpRun {
                timeouts: vec![identity.clone()],
                ..clean_for_policy_test()
            },
            CsharpRun {
                ignored: vec![identity],
                ..clean_for_policy_test()
            },
        ] {
            assert!(validate_csharp_conclusive(&run, Path::new("gate.json")).is_err());
        }
    }

    fn clean_for_policy_test() -> CsharpRun {
        CsharpRun {
            examined_files: vec!["app/FindMyFiles/Engine/PipeProtocol.cs".to_owned()],
            survivors: Vec::new(),
            no_coverage: Vec::new(),
            timeouts: Vec::new(),
            ignored: Vec::new(),
            redundant_ignored: Vec::new(),
            outside_scope: OutsideScopeSummary::default(),
            outside_scope_violations: Vec::new(),
        }
    }

    #[test]
    fn baseline_validation_rejects_duplicates_and_vague_rationales() {
        let base =
            std::env::temp_dir().join(format!("xtask-mutation-baseline-{}", std::process::id()));
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test directory");
        let identity = csharp_identity("app/FindMyFiles/Engine/PipeProtocol.cs", 3);
        let baseline = AcceptedBaseline {
            schema_version: BASELINE_SCHEMA_VERSION,
            tool: expected_tool(STRYKER_NAME, STRYKER_VERSION),
            examined_files: vec!["app/FindMyFiles/Engine/PipeProtocol.cs".to_owned()],
            accepted_equivalents: vec![
                AcceptedEquivalent {
                    identity: identity.clone(),
                    rationale: "equivalent".to_owned(),
                },
                AcceptedEquivalent {
                    identity,
                    rationale: "This is a duplicate identity.".to_owned(),
                },
            ],
        };
        let path = base.join("baseline.json");
        fs::write(
            &path,
            serde_json::to_vec(&baseline).expect("serialize fixture"),
        )
        .expect("write fixture");
        assert!(read_baseline::<CsharpIdentity>(&path, STRYKER_NAME, STRYKER_VERSION).is_err());
        fsx::force_remove_dir_all(&base).expect("remove test directory");
    }

    /// Repository-relative (`/`-separated) paths of every Rust source file
    /// `cargo mutants`, run from `engine/`, can reach.
    ///
    /// `engine/fuzz` is a standalone workspace (like `xtask/`), so the engine
    /// workspace never builds it and cargo-mutants never examines it;
    /// `target`/`build` are generated output, not source.
    fn engine_source_files() -> BTreeSet<String> {
        let repo = paths::repo_root();
        let engine = paths::engine_dir();
        let mut files = BTreeSet::new();
        let mut pending = vec![engine.clone()];
        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
            for entry in entries {
                let entry = entry.expect("read an engine directory entry");
                let kind = entry.file_type().expect("stat an engine directory entry");
                let name = entry.file_name().to_string_lossy().into_owned();
                if kind.is_dir() {
                    if !matches!(name.as_str(), "target" | "build" | "fuzz" | ".git") {
                        pending.push(entry.path());
                    }
                } else if kind.is_file()
                    && Path::new(&name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
                {
                    let relative = entry
                        .path()
                        .strip_prefix(&engine)
                        .expect("engine-relative source path")
                        .to_string_lossy()
                        .into_owned();
                    files.insert(
                        canonical_repo_path(&repo, &engine, &relative)
                            .expect("canonicalize an engine source path"),
                    );
                }
            }
        }
        files
    }

    /// Resolve `engine/mutants.toml`'s `examine_globs` to the exact repository
    /// -relative files they select.
    ///
    /// cargo-mutants owns the real matching at run time and xtask never parses
    /// `mutants.toml`, so — unlike the C# side, where the gate itself reads the
    /// Stryker scope through `read_stryker_scope` — there is no runtime
    /// function to reuse. What makes the resolution exact rather than a second
    /// glob engine is the pinned form the config already documents and this
    /// function enforces: `**/` followed by a wildcard-free path tail
    /// containing at least one `/`, which selects a file if and only if the
    /// file's path ends with `/<tail>`. Anything looser is rejected instead of
    /// approximated.
    fn resolve_examine_globs(config: &str, sources: &BTreeSet<String>) -> Result<Vec<String>> {
        let document = config
            .parse::<toml_edit::DocumentMut>()
            .context("parse the cargo-mutants config")?;
        let globs = document
            .get("examine_globs")
            .and_then(toml_edit::Item::as_array)
            .ok_or_else(|| anyhow!("the cargo-mutants config has no examine_globs array"))?;
        if globs.is_empty() {
            bail!("examine_globs must not be empty");
        }

        let mut scope: BTreeSet<String> = BTreeSet::new();
        for (index, value) in globs.iter().enumerate() {
            let pattern = value
                .as_str()
                .ok_or_else(|| anyhow!("examine_globs[{index}] is not a string"))?;
            let tail = pattern.strip_prefix("**/").ok_or_else(|| {
                anyhow!("examine_globs[{index}] must be `**/` plus an exact path: `{pattern}`")
            })?;
            if tail.is_empty()
                || !tail.contains('/')
                || tail
                    .chars()
                    .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}' | '!'))
            {
                bail!("examine_globs[{index}] is not a reviewable exact path: `{pattern}`");
            }
            let suffix = format!("/{tail}");
            let matched: Vec<&String> = sources
                .iter()
                .filter(|file| file.ends_with(suffix.as_str()))
                .collect();
            if matched.is_empty() {
                bail!("examine_globs[{index}] selects no source file: `{pattern}`");
            }
            for file in matched {
                if !scope.insert(file.clone()) {
                    bail!("examine_globs[{index}] re-selects `{file}`, already in scope");
                }
            }
        }
        Ok(scope.into_iter().collect())
    }

    /// The Rust gate's reviewed scope lives in two files that must agree, and
    /// nothing at run time notices when they don't until `just mutants` has
    /// already burned hours: cargo-mutants derives `examined_files` from what
    /// `examine_globs` actually matched, and only then is it compared with the
    /// baseline. This is that comparison, seconds after the edit.
    #[test]
    fn rust_examine_globs_and_baseline_name_the_same_files() {
        let baseline_path = paths::rust_mutation_baseline();
        let baseline: AcceptedBaseline<RustIdentity> =
            read_baseline(&baseline_path, CARGO_MUTANTS_NAME, CARGO_MUTANTS_VERSION).expect(
                "engine/mutation-baseline.json must be present and well-formed: the Rust \
                 mutation gate reads it before it can compare anything",
            );
        let config_path = paths::engine_dir().join("mutants.toml");
        let config = fs::read_to_string(&config_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", config_path.display()));
        let scope = resolve_examine_globs(&config, &engine_source_files())
            .expect("resolve engine/mutants.toml examine_globs");
        compare_exact_files(&baseline.examined_files, &scope, &baseline_path).expect(
            "engine/mutants.toml examine_globs and engine/mutation-baseline.json \
             examined_files must be edited together",
        );
    }

    /// The C# half of the same invariant, through the very function the gate
    /// calls at run time (`read_stryker_scope` canonicalizes every `mutate`
    /// pattern, rejects wildcards, requires each target to exist on disk, and
    /// ends in the same `compare_exact_files`). Reimplementing that here would
    /// only give the reimplementation something to drift from.
    #[test]
    fn csharp_mutate_scope_and_baseline_name_the_same_files() {
        let repo = paths::repo_root();
        let baseline_path = paths::csharp_mutation_baseline();
        let baseline: AcceptedBaseline<CsharpIdentity> =
            read_baseline(&baseline_path, STRYKER_NAME, STRYKER_VERSION).expect(
                "app/FindMyFiles.Tests/mutation-baseline.json must be present and well-formed: \
                 the C# mutation gate reads it before Stryker is even started",
            );
        read_stryker_scope(
            &repo
                .join("app")
                .join("FindMyFiles.Tests")
                .join("stryker-config.json"),
            &repo,
            &baseline.examined_files,
        )
        .expect(
            "stryker-config.json mutate and app/FindMyFiles.Tests/mutation-baseline.json \
             examined_files must be edited together",
        );
    }

    #[test]
    fn examine_glob_resolution_rejects_loose_dead_and_drifting_globs() {
        let sources = BTreeSet::from([
            "engine/crates/fmf-core/src/query/exec.rs".to_owned(),
            "engine/crates/fmf-core/src/wtf8.rs".to_owned(),
            "engine/crates/fmf-service/src/security.rs".to_owned(),
        ]);
        assert_eq!(
            resolve_examine_globs(
                "examine_globs = [\"**/src/wtf8.rs\", \"**/src/query/exec.rs\"]\n",
                &sources,
            )
            .expect("pinned globs resolve to their exact files"),
            vec![
                "engine/crates/fmf-core/src/query/exec.rs".to_owned(),
                "engine/crates/fmf-core/src/wtf8.rs".to_owned(),
            ]
        );

        for rejected in [
            "timeout_multiplier = 5.0\n",
            "examine_globs = []\n",
            "examine_globs = [\"**/src/*.rs\"]\n",
            "examine_globs = [\"**/wtf8.rs\"]\n",
            "examine_globs = [\"crates/fmf-core/src/wtf8.rs\"]\n",
            "examine_globs = [\"**/src/index/mutate.rs\"]\n",
            "examine_globs = [\"**/src/wtf8.rs\", \"**/src/wtf8.rs\"]\n",
        ] {
            assert!(
                resolve_examine_globs(rejected, &sources).is_err(),
                "unreviewable or dead scope accepted: {rejected}"
            );
        }

        let widened = resolve_examine_globs(
            "examine_globs = [\"**/src/wtf8.rs\", \"**/src/security.rs\"]\n",
            &sources,
        )
        .expect("the widened scope itself is well-formed");
        assert!(compare_exact_files(
            &["engine/crates/fmf-core/src/wtf8.rs".to_owned()],
            &widened,
            Path::new("mutation-baseline.json"),
        )
        .is_err());
    }
}
