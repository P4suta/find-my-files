//! Trusted mutation-CI controller and independent evidence verifier.
//!
//! The reusable workflow checks out the immutable controller and the exact
//! target into separate trees. This module is compiled only from the
//! controller. A target checkout contributes source and tests, but never
//! mutation policy, accepted baselines, task-runner code, Cargo configuration,
//! nextest configuration, Stryker configuration, or tool pins.

use crate::{checksum, fsx, mutation, paths};
use anyhow::{anyhow, bail, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

const RECEIPT_SCHEMA_VERSION: u32 = 1;
const REQUIRED_SHARD_COUNT: usize = 16;
const CARGO_MUTANTS_VERSION: &str = "27.1.0";
const CARGO_NEXTEST_VERSION: &str = "0.9.140";
const DOTNET_SDK_VERSION: &str = "10.0.302";
const STRYKER_VERSION: &str = "4.16.0";
const RUST_TOOLCHAIN_VERSION: &str = "1.97.1";
const CSHARP_TARGET_FRAMEWORK: &str = "net10.0-windows10.0.26100.0";
const POLICY_REVISION: &str = "mutation-controller-v1";
const CSHARP_UNEXECUTED_IGNORE_REASONS: &[&str] = &[
    "Removed by mutate filter",
    "Removed by exclude from code coverage filter",
];
const NEXTEXT_POLICY: &str = r#"nextest-version = "0.9.140"

[store]
dir = "../build/nextest"

[profile.mutation]
fail-fast = true
retries = 0
flaky-result = "fail"
"#;

// `--cargo-arg=--locked` is forwarded by cargo-mutants to both the build and
// nextest invocations. Keep the nextest-only policy here so `--locked` cannot
// accidentally be supplied a second time (nextest rejects duplicate uses).
const RUST_MUTATION_NEXTEST_ARGS: &[&str] = &[
    "--user-config-file",
    "none",
    "--profile",
    "mutation",
    "--fail-fast",
    "--retries",
    "0",
    "--flaky-result",
    "fail",
    "--no-tests",
    "fail",
];

const RUST_REPORT_FILES: &[&str] = &[
    "caught.txt",
    "missed.txt",
    "mutants.json",
    "outcomes.json",
    "timeout.txt",
    "unviable.txt",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RunBinding {
    controller_sha: String,
    target_sha: String,
    run_id: u64,
    run_attempt: u64,
    shard_index: usize,
    shard_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct FileSeal {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct PolicySeal {
    name: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RustTools {
    cargo: String,
    cargo_mutants: String,
    cargo_nextest: String,
    rustc: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CsharpTools {
    cargo: String,
    dotnet_sdk: String,
    dotnet_stryker: String,
    rustc: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct RustMutant {
    name: String,
    package: String,
    path: String,
    line: u64,
    column: u64,
    mutation: String,
}

type CsharpMutant = mutation::CsharpIdentity;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct RustInvalid {
    mutant: RustMutant,
    diagnostic: FileSeal,
    reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct CsharpStatus {
    mutant: CsharpMutant,
    reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RustOutcomes {
    killed: Vec<RustMutant>,
    invalid: Vec<RustInvalid>,
    survived: Vec<RustMutant>,
    timeout: Vec<RustMutant>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CsharpOutcomes {
    killed: Vec<CsharpMutant>,
    invalid: Vec<CsharpStatus>,
    redundant: Vec<CsharpStatus>,
    survived: Vec<CsharpMutant>,
    no_coverage: Vec<CsharpMutant>,
    timeout: Vec<CsharpMutant>,
    ignored: Vec<CsharpStatus>,
    outside_scope_violations: Vec<CsharpStatus>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RustReceipt {
    schema_version: u32,
    language: String,
    binding: RunBinding,
    tools: RustTools,
    policies: Vec<PolicySeal>,
    source_inventory: Vec<SourceFile>,
    source_inventory_sha256: String,
    global_mutants_sha256: String,
    global_mutant_count: usize,
    shard_mutants_sha256: String,
    shard_mutant_count: usize,
    baseline_passed: bool,
    process_exit_code: Option<i32>,
    outcomes: RustOutcomes,
    evidence_files: Vec<FileSeal>,
    passed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CsharpReceipt {
    schema_version: u32,
    language: String,
    binding: RunBinding,
    tools: CsharpTools,
    policies: Vec<PolicySeal>,
    source_inventory: Vec<SourceFile>,
    source_inventory_sha256: String,
    shard_sources: Vec<String>,
    shard_sources_sha256: String,
    tool_executed: bool,
    baseline_passed: bool,
    process_exit_code: Option<i32>,
    outcomes: CsharpOutcomes,
    evidence_files: Vec<FileSeal>,
    passed: bool,
}

/// Optional CI identity for the local mutation commands.
///
/// Supplying none of these options preserves the developer-facing `just
/// mutants`/`just stryker` commands. CI must supply the complete set; a partial
/// identity is rejected rather than silently falling back to the local gate.
#[derive(Args, Clone, Debug, Default)]
pub struct RunArgs {
    /// Exact separately checked-out target repository root.
    #[arg(long)]
    target_root: Option<PathBuf>,
    /// Exact lowercase 40-hex target commit.
    #[arg(long)]
    target_sha: Option<String>,
    /// Exact lowercase 40-hex trusted controller commit.
    #[arg(long)]
    controller_sha: Option<String>,
    /// GitHub Actions workflow run id.
    #[arg(long)]
    run_id: Option<u64>,
    /// GitHub Actions workflow run attempt.
    #[arg(long)]
    run_attempt: Option<u64>,
    /// Zero-based shard index.
    #[arg(long)]
    shard_index: Option<usize>,
    /// Total shard count. The release gate fixes this to sixteen.
    #[arg(long)]
    shard_count: Option<usize>,
}

/// Identity and location required by the fresh, non-executing verifier.
#[derive(Args, Clone, Debug)]
pub struct VerifyArgs {
    /// Exact separately checked-out target repository root.
    #[arg(long)]
    target_root: PathBuf,
    /// Root containing one downloaded artifact directory per shard.
    #[arg(long)]
    evidence_root: PathBuf,
    /// Exact lowercase 40-hex target commit.
    #[arg(long)]
    target_sha: String,
    /// Exact lowercase 40-hex trusted controller commit.
    #[arg(long)]
    controller_sha: String,
    /// GitHub Actions workflow run id.
    #[arg(long)]
    run_id: u64,
    /// GitHub Actions workflow run attempt.
    #[arg(long)]
    run_attempt: u64,
    /// Total shard count. The release gate fixes this to sixteen.
    #[arg(long)]
    shard_count: usize,
}

/// Run the local Rust gate or the trusted CI runner when a complete CI identity
/// is supplied.
pub fn run_rust(args: RunArgs) -> Result<()> {
    match args.into_ci()? {
        Some(args) => run_rust_ci(args),
        None => mutation::run_rust(),
    }
}

/// Run the local C# gate or the trusted CI runner when a complete CI identity
/// is supplied.
pub fn run_csharp(args: RunArgs) -> Result<()> {
    match args.into_ci()? {
        Some(args) => run_csharp_ci(args),
        None => mutation::run_csharp(),
    }
}

/// Verify all Rust shard artifacts without running target code.
pub fn verify_rust(args: VerifyArgs) -> Result<()> {
    validate_verify_args(&args)?;
    verify_rust_evidence(args)
}

/// Verify all C# shard artifacts without running target code.
pub fn verify_csharp(args: VerifyArgs) -> Result<()> {
    validate_verify_args(&args)?;
    verify_csharp_evidence(args)
}

#[derive(Clone, Debug)]
struct CiRunArgs {
    target_root: PathBuf,
    target_sha: String,
    controller_sha: String,
    run_id: u64,
    run_attempt: u64,
    shard_index: usize,
    shard_count: usize,
}

impl RunArgs {
    fn into_ci(self) -> Result<Option<CiRunArgs>> {
        let supplied = [
            self.target_root.is_some(),
            self.target_sha.is_some(),
            self.controller_sha.is_some(),
            self.run_id.is_some(),
            self.run_attempt.is_some(),
            self.shard_index.is_some(),
            self.shard_count.is_some(),
        ];
        if supplied.iter().all(|value| !value) {
            return Ok(None);
        }
        if !supplied.iter().all(|value| *value) {
            bail!(
                "mutation CI identity is all-or-none: target/controller/run/attempt/shard fields are all required"
            );
        }
        let args = CiRunArgs {
            target_root: self
                .target_root
                .ok_or_else(|| anyhow::anyhow!("target root disappeared"))?,
            target_sha: self
                .target_sha
                .ok_or_else(|| anyhow::anyhow!("target SHA disappeared"))?,
            controller_sha: self
                .controller_sha
                .ok_or_else(|| anyhow::anyhow!("controller SHA disappeared"))?,
            run_id: self
                .run_id
                .ok_or_else(|| anyhow::anyhow!("run id disappeared"))?,
            run_attempt: self
                .run_attempt
                .ok_or_else(|| anyhow::anyhow!("run attempt disappeared"))?,
            shard_index: self
                .shard_index
                .ok_or_else(|| anyhow::anyhow!("shard index disappeared"))?,
            shard_count: self
                .shard_count
                .ok_or_else(|| anyhow::anyhow!("shard count disappeared"))?,
        };
        validate_run_args(&args)?;
        Ok(Some(args))
    }
}

fn validate_run_args(args: &CiRunArgs) -> Result<()> {
    validate_identity(
        &args.target_sha,
        &args.controller_sha,
        args.run_id,
        args.run_attempt,
        args.shard_count,
    )?;
    if args.shard_index >= args.shard_count {
        bail!(
            "zero-based shard index {} is outside 0..{}",
            args.shard_index,
            args.shard_count
        );
    }
    Ok(())
}

fn validate_verify_args(args: &VerifyArgs) -> Result<()> {
    validate_identity(
        &args.target_sha,
        &args.controller_sha,
        args.run_id,
        args.run_attempt,
        args.shard_count,
    )
}

fn validate_identity(
    target_sha: &str,
    controller_sha: &str,
    run_id: u64,
    run_attempt: u64,
    shard_count: usize,
) -> Result<()> {
    for (label, sha) in [("target", target_sha), ("controller", controller_sha)] {
        if sha.len() != 40
            || !sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("{label} SHA must be exact lowercase 40-hex");
        }
    }
    if run_id == 0 || run_attempt == 0 {
        bail!("run id and run attempt must both be positive");
    }
    if shard_count != REQUIRED_SHARD_COUNT {
        bail!("mutation release policy requires exactly 16 shards, got {shard_count}");
    }
    Ok(())
}

#[derive(Debug)]
struct Checkout {
    root: PathBuf,
    tracked: Vec<String>,
}

fn controller_root() -> PathBuf {
    paths::repo_root()
}

fn mutation_work_dir(args: &CiRunArgs, language: &str) -> Result<PathBuf> {
    let language_tag = match language {
        "rust" => "r",
        "csharp" => "c",
        other => bail!("unsupported mutation language `{other}`"),
    };
    Ok(paths::mutation_ci_work_dir()
        .join(language_tag)
        .join(format!(
            "{}-{}-{}",
            args.run_id, args.run_attempt, args.shard_index
        )))
}

fn prepare(args: &CiRunArgs, language: &str) -> Result<(Checkout, PathBuf, PathBuf)> {
    let controller = canonical_directory(&controller_root(), "controller root")?;
    let target = canonical_directory(&args.target_root, "target root")?;
    if target == controller || target.starts_with(&controller) || controller.starts_with(&target) {
        bail!(
            "controller and target roots must be separate non-nested trees: controller={}, target={}",
            controller.display(),
            target.display()
        );
    }
    verify_checkout_identity(&controller, &args.controller_sha, "controller")?;
    let checkout = inspect_target_checkout(&target, &args.target_sha)?;

    let work = mutation_work_dir(args, language)?;
    let evidence = paths::mutation_dir().join(language).join(format!(
        "shard-{}-of-{}",
        args.shard_index, args.shard_count
    ));
    reset_owned_directory(&work, &paths::build_root())?;
    reset_owned_directory(&evidence, &paths::build_root())?;
    Ok((checkout, work, evidence))
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("canonicalize {label} {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("{label} is not a directory: {}", canonical.display());
    }
    Ok(canonical)
}

fn reset_owned_directory(path: &Path, build_root: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("owned output has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create mutation output parent {}", parent.display()))?;
    let build = fs::canonicalize(build_root)
        .with_context(|| format!("canonicalize build root {}", build_root.display()))?;
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalize mutation output parent {}", parent.display()))?;
    if parent == build || !parent.starts_with(&build) {
        bail!(
            "refuse recursive cleanup outside a descendant of {}: {}",
            build.display(),
            path.display()
        );
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || fsx::is_reparse_point(&metadata) {
                bail!(
                    "refuse cleanup of a link/reparse-point output root: {}",
                    path.display()
                );
            }
            if metadata.is_dir() {
                reject_links_and_streams(path)?;
            } else if !metadata.is_file() {
                bail!("owned output is a non-file entry: {}", path.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspect owned output before cleanup {}", path.display())
            });
        }
    }
    fsx::force_remove_dir_all(path)
        .with_context(|| format!("remove owned mutation directory {}", path.display()))?;
    fs::create_dir_all(path)
        .with_context(|| format!("create owned mutation directory {}", path.display()))
}

fn verify_checkout_identity(root: &Path, expected_sha: &str, label: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .with_context(|| format!("query {label} checkout identity"))?;
    require_success(&output, &format!("query {label} checkout identity"))?;
    let actual = parse_single_line(&output.stdout, &format!("{label} git HEAD"))?;
    if actual != expected_sha {
        bail!("{label} checkout is {actual}, expected {expected_sha}");
    }
    Ok(())
}

fn inspect_target_checkout(root: &Path, expected_sha: &str) -> Result<Checkout> {
    verify_checkout_identity(root, expected_sha, "target")?;
    reject_links_and_streams(root)?;

    let status = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .context("query exact target checkout status")?;
    require_success(&status, "query exact target checkout status")?;
    if !status.stdout.is_empty() {
        bail!("target checkout is dirty or contains untracked files");
    }

    let listing = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["ls-files", "--cached", "--stage", "-z"])
        .output()
        .context("enumerate exact tracked target files")?;
    require_success(&listing, "enumerate exact tracked target files")?;
    let tracked = parse_tracked_files(root, &listing.stdout)?;
    if tracked.is_empty() {
        bail!("target checkout has no tracked regular files");
    }
    Ok(Checkout {
        root: root.to_path_buf(),
        tracked,
    })
}

fn parse_tracked_files(root: &Path, bytes: &[u8]) -> Result<Vec<String>> {
    if !bytes.ends_with(b"\0") {
        bail!("git ls-files output is not NUL terminated");
    }
    let mut files = Vec::new();
    let mut case_folded = BTreeSet::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record = std::str::from_utf8(record).context("tracked path listing is not UTF-8")?;
        let (metadata, path) = record
            .split_once('\t')
            .ok_or_else(|| anyhow!("malformed git ls-files stage record"))?;
        let mut fields = metadata.split(' ');
        let mode = fields
            .next()
            .ok_or_else(|| anyhow!("tracked record has no mode"))?;
        let oid = fields
            .next()
            .ok_or_else(|| anyhow!("tracked record has no object id"))?;
        let stage = fields
            .next()
            .ok_or_else(|| anyhow!("tracked record has no stage"))?;
        if fields.next().is_some()
            || !matches!(mode, "100644" | "100755")
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            || stage != "0"
        {
            bail!("target contains a non-regular, conflicted, or malformed tracked entry");
        }
        validate_relative_path(path)?;
        let folded = path.to_ascii_lowercase();
        if !case_folded.insert(folded) {
            bail!("target contains case-colliding tracked paths including `{path}`");
        }
        let absolute = root.join(path);
        let metadata = fs::symlink_metadata(&absolute)
            .with_context(|| format!("inspect tracked file {}", absolute.display()))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || fsx::is_reparse_point(&metadata)
        {
            bail!("tracked entry is not a regular non-reparse file: {path}");
        }
        reject_alternate_streams(&absolute)?;
        files.push(path.to_owned());
    }
    files.sort();
    if files.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("tracked file inventory is not strictly unique");
    }
    Ok(files)
}

fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\\')
        || path.starts_with('/')
        || path.chars().any(char::is_control)
    {
        bail!("path is not canonical repository-relative UTF-8: `{path}`");
    }
    let parsed = Path::new(path);
    for component in parsed.components() {
        let Component::Normal(component) = component else {
            bail!("path has a non-normal component: `{path}`");
        };
        let component = component
            .to_str()
            .ok_or_else(|| anyhow!("path component is not UTF-8"))?;
        if component.contains(':')
            || component.ends_with(' ')
            || component.ends_with('.')
            || is_windows_reserved_name(component)
        {
            bail!("path has an unsafe Windows component: `{path}`");
        }
    }
    Ok(())
}

fn is_windows_reserved_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn reject_links_and_streams(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("walk checkout {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || fsx::is_reparse_point(&metadata) {
                bail!(
                    "checkout contains a link or reparse point: {}",
                    path.display()
                );
            }
            reject_alternate_streams(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if !metadata.is_file() {
                bail!("checkout contains a non-file entry: {}", path.display());
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Windows implementation enumerates NTFS streams and can fail; \
              both arms must present one signature to the caller"
)]
const fn reject_alternate_streams(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn reject_alternate_streams(path: &Path) -> Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard, WIN32_FIND_STREAM_DATA,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut data = WIN32_FIND_STREAM_DATA {
        StreamSize: 0,
        cStreamName: [0; 296],
    };
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path and `data` is a
    // correctly initialized output buffer for the synchronous Windows call.
    let handle = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FindStreamInfoStandard,
            (&raw mut data).cast::<c_void>(),
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
            return Ok(());
        }
        return Err(error).with_context(|| format!("enumerate streams for {}", path.display()));
    }
    let result = (|| -> Result<()> {
        loop {
            let end = data
                .cStreamName
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(data.cStreamName.len());
            let name = String::from_utf16(&data.cStreamName[..end])
                .context("alternate stream name is invalid UTF-16")?;
            if name != "::$DATA" {
                bail!(
                    "path has a forbidden alternate data stream `{name}`: {}",
                    path.display()
                );
            }
            // SAFETY: `handle` is live until the scope guard below and `data`
            // remains a valid writable output buffer.
            if unsafe { FindNextStreamW(handle, (&raw mut data).cast::<c_void>()) } == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                    break;
                }
                return Err(error).with_context(|| {
                    format!("continue stream enumeration for {}", path.display())
                });
            }
        }
        Ok(())
    })();
    // SAFETY: `handle` came from FindFirstStreamW and is closed exactly once.
    unsafe {
        CloseHandle(handle);
    }
    result
}

fn require_success(output: &Output, description: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "{description} failed with {}: {}",
        output.status,
        stderr.trim()
    )
}

fn parse_single_line(bytes: &[u8], label: &str) -> Result<String> {
    let text = std::str::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))?;
    let line = text.trim_end_matches(['\r', '\n']);
    if line.is_empty()
        || line.contains(['\r', '\n'])
        || line.trim() != line
        || line.chars().any(char::is_control)
    {
        bail!("{label} is not one canonical non-empty line");
    }
    Ok(line.to_owned())
}

fn source_inventory(checkout: &Checkout, language: &str) -> Result<Vec<SourceFile>> {
    let reviewed_csharp_files = if language == "csharp" {
        Some(
            mutation::read_csharp_reviewed_policy(&controller_root())?
                .examined_files
                .into_iter()
                .collect::<BTreeSet<_>>(),
        )
    } else {
        None
    };
    let mut files = Vec::new();
    for path in &checkout.tracked {
        let selected = match language {
            "rust" => is_rust_production_source(path),
            "csharp" => reviewed_csharp_files
                .as_ref()
                .is_some_and(|reviewed| reviewed.contains(path)),
            _ => false,
        };
        if !selected {
            continue;
        }
        let bytes = fs::read(checkout.root.join(path))
            .with_context(|| format!("read production source `{path}`"))?;
        if language == "rust" && contains_rust_user_ignore(&bytes) {
            bail!(
                "production source `{path}` contains a target-controlled cargo-mutants skip attribute"
            );
        }
        files.push(SourceFile {
            path: path.clone(),
            size: bytes.len() as u64,
            sha256: checksum::sha256_hex(&bytes),
        });
    }
    files.sort();
    if files.is_empty() {
        bail!("{language} production source inventory is empty");
    }
    if let Some(reviewed) = reviewed_csharp_files {
        let actual: BTreeSet<String> = files.iter().map(|source| source.path.clone()).collect();
        if actual != reviewed {
            bail!("target checkout does not contain the exact reviewed C# source inventory");
        }
    }
    Ok(files)
}

fn is_rust_production_source(path: &str) -> bool {
    if !path.starts_with("engine/crates/")
        || !Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
    {
        return false;
    }
    let rest = &path["engine/crates/".len()..];
    rest.contains("/src/") || rest.ends_with("/build.rs")
}

fn contains_rust_user_ignore(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("mutants::skip") || text.contains("mutants(skip")
}

fn copy_target_tree(checkout: &Checkout, work: &Path, language: &str) -> Result<()> {
    for path in &checkout.tracked {
        if !copy_path_for_language(path, language) {
            continue;
        }
        let destination = work.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create sanitized parent {}", parent.display()))?;
        }
        let bytes = fs::read(checkout.root.join(path))
            .with_context(|| format!("read target input `{path}`"))?;
        fs::write(&destination, bytes)
            .with_context(|| format!("write sanitized target input `{path}`"))?;
    }
    Ok(())
}

fn copy_path_for_language(path: &str, language: &str) -> bool {
    match language {
        "rust" => {
            (path.starts_with("engine/")
                || path.starts_with("contract/golden/")
                || path.starts_with("app/FindMyFiles/Assets/")
                || path == "app/FindMyFiles/Engine/Generated/EngineContract.g.cs")
                && !path.starts_with("engine/.cargo/")
                && !path.starts_with("engine/.config/")
                && path != "engine/mutants.toml"
                && path != "engine/mutation-baseline.json"
                && !is_forbidden_auto_config(path)
        }
        "csharp" => {
            (path.starts_with("app/")
                || path.starts_with("contract/golden/")
                || path.starts_with("engine/"))
                && !path.split('/').any(|part| matches!(part, "obj" | "bin"))
                && !path.ends_with("/stryker-config.json")
                && !path.ends_with("/mutation-baseline.json")
                && !path.ends_with("/.editorconfig")
                && !path.starts_with("engine/.cargo/")
                && !path.starts_with("engine/.config/")
                && path != "engine/mutants.toml"
                && path != "engine/mutation-baseline.json"
                && !is_forbidden_auto_config(path)
        }
        _ => false,
    }
}

fn is_forbidden_auto_config(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.eq_ignore_ascii_case("global.json")
        || file.eq_ignore_ascii_case("rust-toolchain")
        || file.eq_ignore_ascii_case("rust-toolchain.toml")
        || file.eq_ignore_ascii_case("nuget.config")
        || file.eq_ignore_ascii_case("directory.build.props")
        || file.eq_ignore_ascii_case("directory.build.targets")
        || file.eq_ignore_ascii_case("directory.packages.props")
        || (path.contains("/.cargo/")
            && matches!(file.to_ascii_lowercase().as_str(), "config" | "config.toml"))
}

fn is_trusted_auto_config(path: &str) -> bool {
    matches!(
        path,
        "engine/.cargo/config.toml" | "xtask/.cargo/config.toml"
    )
}

fn reject_forbidden_auto_configs(checkout: &Checkout) -> Result<()> {
    let mut forbidden = Vec::new();
    for path in &checkout.tracked {
        if !is_forbidden_auto_config(path) {
            continue;
        }
        if is_trusted_auto_config(path) {
            require_target_matches_controller(checkout, path)?;
        } else {
            forbidden.push(path);
        }
    }
    if !forbidden.is_empty() {
        bail!(
            "target contains forbidden auto-discovered tool configuration: {}",
            serde_json::to_string(&forbidden)?
        );
    }
    Ok(())
}

fn require_target_matches_controller(checkout: &Checkout, path: &str) -> Result<Vec<u8>> {
    validate_relative_path(path)?;
    if !checkout.tracked.iter().any(|tracked| tracked == path) {
        bail!("target is missing trusted build-control file `{path}`");
    }
    let controller_bytes = fs::read(controller_root().join(path))
        .with_context(|| format!("read trusted controller file `{path}`"))?;
    let target_bytes = fs::read(checkout.root.join(path))
        .with_context(|| format!("read target build-control file `{path}`"))?;
    if target_bytes != controller_bytes {
        bail!(
            "target-controlled build policy `{path}` differs from the trusted controller; land the policy change on protected main first"
        );
    }
    Ok(controller_bytes)
}

fn rust_build_control_paths() -> Result<Vec<String>> {
    let mut paths = vec![
        "engine/Cargo.lock".to_owned(),
        "engine/Cargo.toml".to_owned(),
    ];
    let crates = controller_root().join("engine").join("crates");
    for entry in fs::read_dir(&crates)
        .with_context(|| format!("enumerate trusted Rust crates {}", crates.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("trusted crate directory is not UTF-8"))?;
        let relative = format!("engine/crates/{name}/Cargo.toml");
        if controller_root().join(&relative).is_file() {
            paths.push(relative);
        }
    }
    paths.sort();
    Ok(paths)
}

fn rust_policies(checkout: &Checkout, work: &Path) -> Result<Vec<PolicySeal>> {
    let mut policies = vec![policy_seal("runner-revision", POLICY_REVISION.as_bytes())];
    let mise = fs::read(controller_root().join("mise.toml")).context("read trusted mise.toml")?;
    validate_mise_rust_pin(&mise)?;
    policies.push(policy_seal("controller:mise.toml", &mise));

    let cargo_config = require_target_matches_controller(checkout, "engine/.cargo/config.toml")?;
    write_bytes(
        &work.join("engine").join(".cargo").join("config.toml"),
        &cargo_config,
    )?;
    policies.push(policy_seal(
        "controller:engine/.cargo/config.toml",
        &cargo_config,
    ));

    write_bytes(
        &work
            .join("engine")
            .join(".config")
            .join("nextest-mutation.toml"),
        NEXTEXT_POLICY.as_bytes(),
    )?;
    policies.push(policy_seal(
        "embedded:nextest-mutation.toml",
        NEXTEXT_POLICY.as_bytes(),
    ));

    for path in rust_build_control_paths()? {
        let bytes = require_target_matches_controller(checkout, &path)?;
        policies.push(policy_seal(format!("controller:{path}"), &bytes));
    }
    policies.sort();
    Ok(policies)
}

fn validate_mise_rust_pin(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).context("trusted mise.toml is not UTF-8")?;
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .context("parse trusted mise.toml")?;
    let actual = document["tools"]["rust"]
        .as_str()
        .ok_or_else(|| anyhow!("trusted mise.toml has no tools.rust string"))?;
    if actual != RUST_TOOLCHAIN_VERSION {
        bail!("trusted Rust pin drift: expected {RUST_TOOLCHAIN_VERSION}, got {actual}");
    }
    Ok(())
}

const fn csharp_build_control_paths() -> &'static [&'static str] {
    &[
        "app/FindMyFiles/FindMyFiles.csproj",
        "app/FindMyFiles/packages.lock.json",
        "app/FindMyFiles.Tests/FindMyFiles.Tests.csproj",
        "app/FindMyFiles.Tests/packages.lock.json",
    ]
}

fn csharp_policies(checkout: &Checkout, work: &Path) -> Result<Vec<PolicySeal>> {
    let mut policies = vec![policy_seal("runner-revision", POLICY_REVISION.as_bytes())];
    for path in csharp_build_control_paths() {
        let bytes = require_target_matches_controller(checkout, path)?;
        policies.push(policy_seal(format!("controller:{path}"), &bytes));
    }
    for path in [
        "app/FindMyFiles.Tests/stryker-config.json",
        "app/FindMyFiles.Tests/mutation-baseline.json",
    ] {
        let bytes = require_target_matches_controller(checkout, path)?;
        policies.push(policy_seal(format!("controller:{path}"), &bytes));
    }
    let mise = fs::read(controller_root().join("mise.toml")).context("read trusted mise.toml")?;
    validate_mise_rust_pin(&mise)?;
    policies.push(policy_seal("controller:mise.toml", &mise));
    let cargo_config = require_target_matches_controller(checkout, "engine/.cargo/config.toml")?;
    write_bytes(
        &work.join("engine").join(".cargo").join("config.toml"),
        &cargo_config,
    )?;
    policies.push(policy_seal(
        "controller:engine/.cargo/config.toml",
        &cargo_config,
    ));
    for path in rust_build_control_paths()? {
        let bytes = require_target_matches_controller(checkout, &path)?;
        policies.push(policy_seal(format!("controller:{path}"), &bytes));
    }

    let tool_manifest = fs::read(controller_root().join(".config").join("dotnet-tools.json"))
        .context("read trusted dotnet tool manifest")?;
    validate_stryker_manifest(&tool_manifest)?;
    write_bytes(
        &work.join(".config").join("dotnet-tools.json"),
        &tool_manifest,
    )?;
    policies.push(policy_seal(
        "controller:.config/dotnet-tools.json",
        &tool_manifest,
    ));

    let editorconfig =
        fs::read(controller_root().join(".editorconfig")).context("read trusted .editorconfig")?;
    write_bytes(&work.join(".editorconfig"), &editorconfig)?;
    policies.push(policy_seal("controller:.editorconfig", &editorconfig));
    policies.sort();
    Ok(policies)
}

fn validate_stryker_manifest(bytes: &[u8]) -> Result<()> {
    let temporary = paths::build_root()
        .join("mutation")
        .join("manifest-validation.json");
    write_bytes(&temporary, bytes)?;
    let value: Value = mutation::read_json(&temporary)?;
    let expected = value
        .pointer("/tools/dotnet-stryker")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("trusted dotnet tool manifest has no dotnet-stryker object"))?;
    if expected.get("version").and_then(Value::as_str) != Some(STRYKER_VERSION)
        || expected.get("rollForward").and_then(Value::as_bool) != Some(false)
        || expected
            .get("commands")
            .and_then(Value::as_array)
            .is_none_or(|commands| {
                commands.len() != 1 || commands[0].as_str() != Some("dotnet-stryker")
            })
    {
        bail!(
            "trusted dotnet tool manifest must pin only dotnet-stryker {STRYKER_VERSION} with rollForward=false"
        );
    }
    fs::remove_file(&temporary)
        .with_context(|| format!("remove manifest validation file {}", temporary.display()))?;
    Ok(())
}

fn rust_tools(work: &Path) -> Result<RustTools> {
    let rustc = capture_text(rust_command("rustc", work), &["--version"], "rustc version")?;
    let cargo = capture_text(rust_command("cargo", work), &["--version"], "cargo version")?;
    let cargo_mutants = capture_text(
        rust_command("cargo", work),
        &["mutants", "--version"],
        "cargo-mutants version",
    )?;
    let cargo_nextest = capture_text(
        rust_command("cargo", work),
        &["nextest", "--version"],
        "cargo-nextest version",
    )?;
    validate_prefixed_version(
        &first_line(&rustc)?,
        &format!("rustc {RUST_TOOLCHAIN_VERSION}"),
        "rustc",
    )?;
    validate_prefixed_version(
        &first_line(&cargo)?,
        &format!("cargo {RUST_TOOLCHAIN_VERSION}"),
        "cargo",
    )?;
    if first_line(&cargo_mutants)? != format!("cargo-mutants {CARGO_MUTANTS_VERSION}") {
        bail!("cargo-mutants runtime pin mismatch");
    }
    if first_line(&cargo_nextest)? != format!("cargo-nextest {CARGO_NEXTEST_VERSION}")
        && !first_line(&cargo_nextest)?
            .starts_with(&format!("cargo-nextest {CARGO_NEXTEST_VERSION} "))
    {
        bail!("cargo-nextest runtime pin mismatch");
    }
    Ok(RustTools {
        cargo,
        cargo_mutants,
        cargo_nextest,
        rustc,
    })
}

fn restore_and_verify_csharp_tools(work: &Path) -> Result<CsharpTools> {
    let rustc = capture_text(
        rust_command("rustc", work),
        &["--version"],
        "C# lane rustc version",
    )?;
    let cargo = capture_text(
        rust_command("cargo", work),
        &["--version"],
        "C# lane cargo version",
    )?;
    validate_prefixed_version(
        &rustc,
        &format!("rustc {RUST_TOOLCHAIN_VERSION}"),
        "C# lane rustc",
    )?;
    validate_prefixed_version(
        &cargo,
        &format!("cargo {RUST_TOOLCHAIN_VERSION}"),
        "C# lane cargo",
    )?;
    let dotnet_sdk = capture_text(dotnet_command(work), &["--version"], ".NET SDK version")?;
    if dotnet_sdk != DOTNET_SDK_VERSION {
        bail!(".NET SDK runtime pin mismatch: expected {DOTNET_SDK_VERSION}, got {dotnet_sdk}");
    }
    let manifest = work.join(".config").join("dotnet-tools.json");
    let manifest_arg = path_arg(&manifest)?;
    let mut restore = dotnet_command(work);
    restore.args(["tool", "restore", "--tool-manifest", &manifest_arg]);
    run_status_required(&mut restore, "restore pinned Stryker tool")?;

    let list = capture_text(
        dotnet_command(work),
        &["tool", "list", "--local", "--format", "json"],
        "restored dotnet tool inventory",
    )?;
    let value: Value = serde_json::from_str(&list).context("parse dotnet tool inventory JSON")?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("dotnet tool inventory has no data array"))?;
    if data.len() != 1
        || data[0].get("packageId").and_then(Value::as_str) != Some("dotnet-stryker")
        || data[0].get("version").and_then(Value::as_str) != Some(STRYKER_VERSION)
        || data[0]
            .get("commands")
            .and_then(Value::as_array)
            .is_none_or(|commands| {
                commands.len() != 1 || commands[0].as_str() != Some("dotnet-stryker")
            })
    {
        bail!("restored local tool inventory is not exactly dotnet-stryker {STRYKER_VERSION}");
    }
    Ok(CsharpTools {
        cargo,
        dotnet_sdk,
        dotnet_stryker: format!("dotnet-stryker {STRYKER_VERSION}"),
        rustc,
    })
}

fn capture_text(mut command: Command, args: &[&str], label: &str) -> Result<String> {
    let output = command
        .args(args)
        .output()
        .with_context(|| format!("query {label}"))?;
    require_success(&output, &format!("query {label}"))?;
    let text = std::str::from_utf8(&output.stdout)
        .with_context(|| format!("{label} is not UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    if text.is_empty() || text.chars().any(|character| character == '\0') {
        bail!("{label} is empty or contains NUL");
    }
    Ok(text.replace("\r\n", "\n"))
}

fn first_line(text: &str) -> Result<String> {
    let line = text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("tool version output has no first line"))?;
    if line.trim() != line || line.chars().any(char::is_control) {
        bail!("tool version first line is not canonical");
    }
    Ok(line.to_owned())
}

fn path_arg(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("path is not UTF-8: {}", path.display()))
}

fn run_status_required(command: &mut Command, label: &str) -> Result<()> {
    let status = command.status().with_context(|| format!("spawn {label}"))?;
    if !status.success() {
        bail!("{label} failed with {status}");
    }
    Ok(())
}

#[derive(Debug)]
struct ParsedRustRun {
    baseline_log: String,
    baseline_passed: bool,
    killed: Vec<RustMutant>,
    invalid: Vec<(RustMutant, String)>,
    survived: Vec<RustMutant>,
    timeout: Vec<RustMutant>,
}

fn run_rust_ci(args: CiRunArgs) -> Result<()> {
    let (checkout, work, evidence) = prepare(&args, "rust")?;
    reject_forbidden_auto_configs(&checkout)?;
    let sources = source_inventory(&checkout, "rust")?;
    copy_target_tree(&checkout, &work, "rust")?;
    let policies = rust_policies(&checkout, &work)?;
    let tools = rust_tools(&work)?;

    let global_output = run_rust_list(&work)?;
    write_bytes(&evidence.join("global-mutants.json"), &global_output.stdout)?;
    let global = parse_rust_mutant_list(&evidence.join("global-mutants.json"), true)?;
    require_generated_sources_in_inventory(&global, &sources, "Rust")?;
    let expected_shard: BTreeSet<RustMutant> = global
        .iter()
        .enumerate()
        .filter(|(index, _)| index % args.shard_count == args.shard_index)
        .map(|(_, mutant)| mutant.clone())
        .collect();

    let raw_parent = work.join("tool-output").join("rust");
    reset_owned_directory(&raw_parent, &paths::build_root())?;
    let raw_parent_arg = path_arg(&raw_parent)?;
    let nextest_config = work
        .join("engine")
        .join(".config")
        .join("nextest-mutation.toml");
    let nextest_config_arg = path_arg(&nextest_config)?;
    let shard = format!("{}/{}", args.shard_index, args.shard_count);
    let mut command = rust_command("cargo", &work);
    command.args([
        "mutants",
        "--no-config",
        "--workspace",
        "--output",
        &raw_parent_arg,
        "--baseline",
        "run",
        "--no-shuffle",
        "--no-times",
        "--colors",
        "never",
        "--annotations",
        "none",
        "--cargo-arg=--locked",
        "--test-workspace",
        "true",
        "--test-tool",
        "nextest",
        "--timeout-multiplier",
        "5.0",
        "--minimum-test-timeout",
        "60",
        "--skip-calls-defaults",
        "false",
        "--shard",
        &shard,
        "--sharding",
        "round-robin",
        "--",
        "--config-file",
        &nextest_config_arg,
    ]);
    command.args(RUST_MUTATION_NEXTEST_ARGS);
    let status = command
        .status()
        .context("spawn trusted cargo-mutants run")?;
    let native = mutation::cargo_mutants_report_dir(&raw_parent);
    for name in RUST_REPORT_FILES {
        let bytes = fs::read(native.join(name))
            .with_context(|| format!("read cargo-mutants report `{name}`"))?;
        write_bytes(&evidence.join(name), &bytes)?;
    }

    let shard_mutants = parse_rust_mutant_list(&evidence.join("mutants.json"), false)?;
    let shard_set: BTreeSet<RustMutant> = shard_mutants.iter().cloned().collect();
    if shard_set != expected_shard || shard_mutants.len() != shard_set.len() {
        bail!(
            "cargo-mutants shard {} does not equal the trusted round-robin partition",
            args.shard_index
        );
    }
    let parsed = parse_rust_outcomes(&evidence, &shard_set)?;
    let mut invalid = Vec::new();
    for (mutant, log_path) in parsed.invalid {
        let source_log = safe_report_path(&native, &log_path, "cargo-mutants log")?;
        let bytes = fs::read(&source_log)
            .with_context(|| format!("read Unviable diagnostic {}", source_log.display()))?;
        if bytes.is_empty() {
            bail!(
                "Unviable mutant `{}` has an empty diagnostic log",
                mutant.name
            );
        }
        let identity_hash = checksum::sha256_hex(&serde_json::to_vec(&mutant)?);
        let relative = format!("invalid/{identity_hash}.log");
        write_bytes(&evidence.join(&relative), &bytes)?;
        invalid.push(RustInvalid {
            mutant,
            diagnostic: seal_file(&evidence, &relative)?,
            reason: "Unviable".to_owned(),
        });
    }
    invalid.sort();

    let baseline_source = safe_report_path(&native, &parsed.baseline_log, "baseline log")?;
    let baseline_bytes = fs::read(&baseline_source)
        .with_context(|| format!("read baseline log {}", baseline_source.display()))?;
    if baseline_bytes.is_empty() {
        bail!("cargo-mutants baseline log is empty");
    }
    write_bytes(&evidence.join("baseline.log"), &baseline_bytes)?;

    if !parsed.baseline_passed {
        bail!(
            "cargo-mutants unmutated baseline was not successful; diagnostic={}",
            evidence.join("baseline.log").display()
        );
    }

    ensure_hash(
        &nextest_config,
        &checksum::sha256_hex(NEXTEXT_POLICY.as_bytes()),
        "trusted nextest policy",
    )?;
    let gate_passed = parsed.baseline_passed
        && parsed.survived.is_empty()
        && parsed.timeout.is_empty()
        && status.code() == Some(0);
    let outcomes = RustOutcomes {
        killed: parsed.killed,
        invalid,
        survived: parsed.survived,
        timeout: parsed.timeout,
    };
    validate_rust_terminal_partition(&shard_set, &outcomes)?;

    let mut receipt = RustReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        language: "rust".to_owned(),
        binding: binding(&args),
        tools,
        policies,
        source_inventory_sha256: inventory_hash(&sources)?,
        source_inventory: sources,
        global_mutants_sha256: inventory_hash(&global)?,
        global_mutant_count: global.len(),
        shard_mutants_sha256: inventory_hash(&shard_mutants)?,
        shard_mutant_count: shard_mutants.len(),
        baseline_passed: parsed.baseline_passed,
        process_exit_code: status.code(),
        outcomes,
        evidence_files: Vec::new(),
        passed: gate_passed,
    };
    receipt.evidence_files = seal_all_evidence_files(&evidence)?;
    mutation::write_json_atomic(&evidence.join("receipt.json"), &receipt)?;
    verify_file_seals(&evidence, &receipt.evidence_files)?;

    if !receipt.passed {
        bail!(
            "Rust mutation shard {} failed closed: survived={}, timeout={}, baseline={}, exit={:?}; evidence={}",
            args.shard_index,
            receipt.outcomes.survived.len(),
            receipt.outcomes.timeout.len(),
            receipt.baseline_passed,
            receipt.process_exit_code,
            evidence.display()
        );
    }
    println!(
        "Rust mutation shard {}/{} passed: {} valid killed, {} invalid recorded.",
        args.shard_index,
        args.shard_count,
        receipt.outcomes.killed.len(),
        receipt.outcomes.invalid.len()
    );
    Ok(())
}

fn binding(args: &CiRunArgs) -> RunBinding {
    RunBinding {
        controller_sha: args.controller_sha.clone(),
        target_sha: args.target_sha.clone(),
        run_id: args.run_id,
        run_attempt: args.run_attempt,
        shard_index: args.shard_index,
        shard_count: args.shard_count,
    }
}

fn run_rust_list(work: &Path) -> Result<Output> {
    let mut command = rust_command("cargo", work);
    let output = command
        .args([
            "mutants",
            "--no-config",
            "--workspace",
            "--list",
            "--json",
            "--no-shuffle",
            "--no-times",
            "--colors",
            "never",
            "--annotations",
            "none",
            "--cargo-arg=--locked",
            "--skip-calls-defaults",
            "false",
        ])
        .output()
        .context("enumerate complete Rust mutant inventory")?;
    require_success(&output, "enumerate complete Rust mutant inventory")?;
    if output.stdout.is_empty() {
        bail!("cargo-mutants returned an empty global JSON inventory");
    }
    Ok(output)
}

fn parse_rust_mutant_list(path: &Path, require_nonempty: bool) -> Result<Vec<RustMutant>> {
    let value: Value = mutation::read_json(path)?;
    let entries = value
        .as_array()
        .ok_or_else(|| anyhow!("{} root is not a mutant array", path.display()))?;
    if require_nonempty && entries.is_empty() {
        bail!("{} contains no generated mutants", path.display());
    }
    let mut mutants = Vec::with_capacity(entries.len());
    let mut unique = BTreeSet::new();
    for (index, value) in entries.iter().enumerate() {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("{} mutant #{index} is not an object", path.display()))?;
        assert_exact_keys(
            object,
            &[
                "diff",
                "file",
                "function",
                "genre",
                "name",
                "package",
                "replacement",
                "span",
            ],
            &format!("{} mutant #{index}", path.display()),
        )?;
        let mutant = parse_rust_mutant(object, true)?;
        if !unique.insert(mutant.clone()) {
            bail!(
                "{} contains duplicate mutant `{}`",
                path.display(),
                mutant.name
            );
        }
        mutants.push(mutant);
    }
    Ok(mutants)
}

fn parse_rust_mutant(object: &Map<String, Value>, listing: bool) -> Result<RustMutant> {
    let expected = if listing {
        &[
            "diff",
            "file",
            "function",
            "genre",
            "name",
            "package",
            "replacement",
            "span",
        ][..]
    } else {
        &[
            "file",
            "function",
            "genre",
            "name",
            "package",
            "replacement",
            "span",
        ][..]
    };
    assert_exact_keys(object, expected, "cargo-mutants mutant")?;
    let file = clean_string(object.get("file"), "mutant.file", false)?;
    validate_relative_path(&file)?;
    let path = format!("engine/{file}");
    validate_relative_path(&path)?;
    let package = clean_string(object.get("package"), "mutant.package", false)?;
    let name = clean_string(object.get("name"), "mutant.name", false)?;
    let span = exact_object(object.get("span"), "mutant.span", &["end", "start"])?;
    let start = exact_object(span.get("start"), "mutant.span.start", &["column", "line"])?;
    let end = exact_object(span.get("end"), "mutant.span.end", &["column", "line"])?;
    let line = positive_u64(start.get("line"), "mutant.span.start.line")?;
    let column = positive_u64(start.get("column"), "mutant.span.start.column")?;
    let end_line = positive_u64(end.get("line"), "mutant.span.end.line")?;
    let end_column = positive_u64(end.get("column"), "mutant.span.end.column")?;
    if (end_line, end_column) < (line, column) {
        bail!("cargo-mutants source span ends before it starts");
    }
    let prefix = format!("{file}:{line}:{column}: ");
    let mutation = name
        .strip_prefix(&prefix)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("cargo-mutants name does not match its exact file/span"))?
        .to_owned();
    clean_string(object.get("genre"), "mutant.genre", false)?;
    arbitrary_string(object.get("replacement"), "mutant.replacement", true)?;
    if listing {
        arbitrary_string(object.get("diff"), "mutant.diff", false)?;
    }
    Ok(RustMutant {
        name,
        package,
        path,
        line,
        column,
        mutation,
    })
}

fn parse_rust_outcomes(evidence: &Path, generated: &BTreeSet<RustMutant>) -> Result<ParsedRustRun> {
    let path = evidence.join("outcomes.json");
    let root: Value = mutation::read_json(&path)?;
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("{} root is not an object", path.display()))?;
    assert_exact_keys(
        root,
        &[
            "cargo_mutants_version",
            "caught",
            "end_time",
            "missed",
            "outcomes",
            "start_time",
            "success",
            "timeout",
            "total_mutants",
            "unviable",
        ],
        "cargo-mutants outcomes root",
    )?;
    if root.get("cargo_mutants_version").and_then(Value::as_str) != Some(CARGO_MUTANTS_VERSION) {
        bail!("outcomes.json cargo-mutants version pin mismatch");
    }
    let entries = root
        .get("outcomes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("outcomes.json has no outcomes array"))?;
    let mut baseline_count = 0_usize;
    let mut baseline_log = None;
    let mut baseline_passed = None;
    let mut killed = BTreeSet::new();
    let mut invalid = BTreeMap::new();
    let mut survived = BTreeSet::new();
    let mut timeout = BTreeSet::new();
    for (index, value) in entries.iter().enumerate() {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("outcome #{index} is not an object"))?;
        assert_exact_keys(
            object,
            &[
                "diff_path",
                "log_path",
                "phase_results",
                "scenario",
                "summary",
            ],
            &format!("cargo-mutants outcome #{index}"),
        )?;
        let summary = clean_string(object.get("summary"), "outcome.summary", false)?;
        let log_path = clean_string(object.get("log_path"), "outcome.log_path", false)?;
        validate_report_relative_path(&log_path)?;
        let scenario = object
            .get("scenario")
            .ok_or_else(|| anyhow!("outcome has no scenario"))?;
        if scenario.as_str() == Some("Baseline") {
            baseline_count += 1;
            baseline_passed = Some(parse_rust_baseline_summary(&summary)?);
            baseline_log = Some(log_path);
            continue;
        }
        let mutant_object = scenario
            .get("Mutant")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("outcome #{index} has an unknown scenario"))?;
        let mutant = parse_rust_mutant(mutant_object, false)?;
        match summary.as_str() {
            "CaughtMutant" => {
                if !killed.insert(mutant) {
                    bail!("duplicate caught Rust mutant outcome");
                }
            }
            "Unviable" => {
                if invalid.insert(mutant, log_path).is_some() {
                    bail!("duplicate Unviable Rust mutant outcome");
                }
            }
            "MissedMutant" => {
                if !survived.insert(mutant) {
                    bail!("duplicate survived Rust mutant outcome");
                }
            }
            "Timeout" => {
                if !timeout.insert(mutant) {
                    bail!("duplicate timeout Rust mutant outcome");
                }
            }
            other => bail!("cargo-mutants emitted unknown/non-terminal summary `{other}`"),
        }
    }
    if baseline_count != 1 {
        bail!("outcomes.json must contain exactly one baseline");
    }
    let baseline_passed = baseline_passed.ok_or_else(|| anyhow!("baseline result disappeared"))?;
    let all = validate_parsed_rust_partition(
        baseline_passed,
        generated,
        &killed,
        &invalid,
        &survived,
        &timeout,
    )?;
    let killed_vec: Vec<_> = killed.into_iter().collect();
    let invalid_vec: Vec<_> = invalid.into_iter().collect();
    let survived_vec: Vec<_> = survived.into_iter().collect();
    let timeout_vec: Vec<_> = timeout.into_iter().collect();
    validate_rust_text_report(evidence, "caught.txt", &killed_vec)?;
    validate_rust_text_report(
        evidence,
        "unviable.txt",
        &invalid_vec
            .iter()
            .map(|(mutant, _)| mutant.clone())
            .collect::<Vec<_>>(),
    )?;
    validate_rust_text_report(evidence, "missed.txt", &survived_vec)?;
    validate_rust_text_report(evidence, "timeout.txt", &timeout_vec)?;
    validate_count(root, "total_mutants", all.len())?;
    validate_count(root, "caught", killed_vec.len())?;
    validate_count(root, "unviable", invalid_vec.len())?;
    validate_count(root, "missed", survived_vec.len())?;
    validate_count(root, "timeout", timeout_vec.len())?;
    Ok(ParsedRustRun {
        baseline_log: baseline_log.ok_or_else(|| anyhow!("baseline log path disappeared"))?,
        baseline_passed,
        killed: killed_vec,
        invalid: invalid_vec,
        survived: survived_vec,
        timeout: timeout_vec,
    })
}

fn validate_parsed_rust_partition(
    baseline_passed: bool,
    generated: &BTreeSet<RustMutant>,
    killed: &BTreeSet<RustMutant>,
    invalid: &BTreeMap<RustMutant, String>,
    survived: &BTreeSet<RustMutant>,
    timeout: &BTreeSet<RustMutant>,
) -> Result<BTreeSet<RustMutant>> {
    let all: BTreeSet<RustMutant> = killed
        .iter()
        .chain(invalid.keys())
        .chain(survived)
        .chain(timeout)
        .cloned()
        .collect();
    if all.len() != killed.len() + invalid.len() + survived.len() + timeout.len() {
        bail!("Rust terminal outcomes are not disjoint");
    }
    if baseline_passed {
        if &all != generated {
            bail!("Rust terminal outcomes are not an exhaustive generated-mutant partition");
        }
    } else if !all.is_empty() {
        bail!("failed Rust baseline unexpectedly emitted mutant outcomes");
    }
    Ok(all)
}

fn parse_rust_baseline_summary(summary: &str) -> Result<bool> {
    match summary {
        "Success" => Ok(true),
        "Failure" => Ok(false),
        other => bail!("cargo-mutants emitted unknown baseline summary `{other}`"),
    }
}

fn validate_rust_text_report(evidence: &Path, name: &str, expected: &[RustMutant]) -> Result<()> {
    let bytes = fs::read(evidence.join(name))
        .with_context(|| format!("read cargo-mutants text report `{name}`"))?;
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("cargo-mutants text report `{name}` is not UTF-8"))?;
    let mut actual = BTreeSet::new();
    for line in text.lines() {
        if line.is_empty() || line.trim() != line || line.chars().any(char::is_control) {
            bail!("cargo-mutants text report `{name}` has a malformed line");
        }
        if !actual.insert(line.to_owned()) {
            bail!("cargo-mutants text report `{name}` has a duplicate line");
        }
    }
    let expected: BTreeSet<String> = expected.iter().map(|mutant| mutant.name.clone()).collect();
    if actual != expected {
        bail!("cargo-mutants text report `{name}` disagrees with outcomes.json");
    }
    Ok(())
}

fn validate_count(root: &Map<String, Value>, field: &str, expected: usize) -> Result<()> {
    let actual = root
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("outcomes.json `{field}` is not a non-negative integer"))?;
    if actual != expected as u64 {
        bail!("outcomes.json `{field}` is {actual}, expected {expected}");
    }
    Ok(())
}

fn validate_rust_terminal_partition(
    generated: &BTreeSet<RustMutant>,
    outcomes: &RustOutcomes,
) -> Result<()> {
    let invalid: Vec<RustMutant> = outcomes
        .invalid
        .iter()
        .map(|entry| entry.mutant.clone())
        .collect();
    let all: BTreeSet<RustMutant> = outcomes
        .killed
        .iter()
        .chain(&invalid)
        .chain(&outcomes.survived)
        .chain(&outcomes.timeout)
        .cloned()
        .collect();
    let total =
        outcomes.killed.len() + invalid.len() + outcomes.survived.len() + outcomes.timeout.len();
    if &all != generated || all.len() != total {
        bail!("Rust receipt outcomes are not disjoint and exhaustive");
    }
    Ok(())
}

fn require_generated_sources_in_inventory(
    mutants: &[RustMutant],
    sources: &[SourceFile],
    language: &str,
) -> Result<()> {
    let inventory: BTreeSet<&str> = sources.iter().map(|source| source.path.as_str()).collect();
    let outside: BTreeSet<&str> = mutants
        .iter()
        .map(|mutant| mutant.path.as_str())
        .filter(|path| !inventory.contains(path))
        .collect();
    if !outside.is_empty() {
        bail!(
            "{language} generated mutants outside the machine-derived production inventory: {}",
            serde_json::to_string(&outside)?
        );
    }
    Ok(())
}

fn safe_report_path(root: &Path, relative: &str, label: &str) -> Result<PathBuf> {
    validate_report_relative_path(relative)?;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || fsx::is_reparse_point(&metadata)
    {
        bail!(
            "{label} is not a regular non-reparse file: {}",
            path.display()
        );
    }
    reject_alternate_streams(&path)?;
    Ok(path)
}

fn validate_report_relative_path(path: &str) -> Result<()> {
    validate_relative_path(&path.replace('\\', "/"))?;
    if path.contains('\\') {
        bail!("report path must use forward slashes: `{path}`");
    }
    Ok(())
}

fn ensure_hash(path: &Path, expected: &str, label: &str) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("read {label} {}", path.display()))?;
    let actual = checksum::sha256_hex(&bytes);
    if actual != expected {
        bail!("{label} changed while target code was executing");
    }
    Ok(())
}

fn verify_file_seals(root: &Path, expected: &[FileSeal]) -> Result<()> {
    let mut actual_paths = Vec::new();
    collect_regular_files(root, root, &mut actual_paths)?;
    actual_paths.retain(|path| path != "receipt.json");
    actual_paths.sort();
    let expected_paths: Vec<&str> = expected.iter().map(|seal| seal.path.as_str()).collect();
    if actual_paths.iter().map(String::as_str).collect::<Vec<_>>() != expected_paths {
        bail!("evidence file set differs from its exact receipt");
    }
    for seal in expected {
        validate_sha256(&seal.sha256, "evidence file hash")?;
        let actual = seal_file(root, &seal.path)?;
        if actual != *seal {
            bail!(
                "evidence file `{}` differs from its receipt seal",
                seal.path
            );
        }
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} is not lowercase SHA-256");
    }
    Ok(())
}

fn assert_exact_keys(object: &Map<String, Value>, expected: &[&str], label: &str) -> Result<()> {
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    if actual != expected {
        bail!(
            "{label} JSON keys drifted: actual={}, expected={}",
            serde_json::to_string(&actual)?,
            serde_json::to_string(&expected)?
        );
    }
    Ok(())
}

fn assert_required_optional_keys(
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    label: &str,
) -> Result<()> {
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let required: BTreeSet<&str> = required.iter().copied().collect();
    let optional: BTreeSet<&str> = optional.iter().copied().collect();
    if !required.is_disjoint(&optional) {
        bail!("{label} controller key policy overlaps");
    }
    let allowed: BTreeSet<&str> = required.union(&optional).copied().collect();
    if !required.is_subset(&actual) || !actual.is_subset(&allowed) {
        bail!(
            "{label} JSON keys drifted: actual={}, required={}, optional={}",
            serde_json::to_string(&actual)?,
            serde_json::to_string(&required)?,
            serde_json::to_string(&optional)?
        );
    }
    Ok(())
}

fn exact_object<'a>(
    value: Option<&'a Value>,
    label: &str,
    keys: &[&str],
) -> Result<&'a Map<String, Value>> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{label} is not an object"))?;
    assert_exact_keys(object, keys, label)?;
    Ok(object)
}

fn clean_string(value: Option<&Value>, label: &str, allow_empty: bool) -> Result<String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{label} is not a string"))?;
    if value.chars().any(|character| character == '\0')
        || value.trim() != value
        || (!allow_empty && value.is_empty())
    {
        bail!("{label} is not canonical");
    }
    Ok(value.to_owned())
}

fn arbitrary_string(value: Option<&Value>, label: &str, allow_empty: bool) -> Result<String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{label} is not a string"))?;
    if value.chars().any(|character| character == '\0') || (!allow_empty && value.is_empty()) {
        bail!("{label} is empty or contains NUL");
    }
    Ok(value.to_owned())
}

fn positive_u64(value: Option<&Value>, label: &str) -> Result<u64> {
    let value = value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{label} is not a non-negative integer"))?;
    if value == 0 {
        bail!("{label} must be positive");
    }
    Ok(value)
}

fn run_csharp_ci(args: CiRunArgs) -> Result<()> {
    let (checkout, work, evidence) = prepare(&args, "csharp")?;
    reject_forbidden_auto_configs(&checkout)?;
    let reviewed_policy = mutation::read_csharp_reviewed_policy(&controller_root())?;
    let sources = source_inventory(&checkout, "csharp")?;
    copy_target_tree(&checkout, &work, "csharp")?;
    let mut policies = csharp_policies(&checkout, &work)?;
    let tools = restore_and_verify_csharp_tools(&work)?;
    build_csharp_native_dependency(&work)?;
    let shard_sources: Vec<String> = sources
        .iter()
        .enumerate()
        .filter(|(index, _)| index % args.shard_count == args.shard_index)
        .map(|(_, source)| source.path.clone())
        .collect();
    let expected_survivors = reviewed_csharp_survivors(&reviewed_policy, &shard_sources);

    let restore_output = run_dotnet_project_restore(&work)?;
    write_bytes(&evidence.join("restore.stdout.log"), &restore_output.stdout)?;
    write_bytes(&evidence.join("restore.stderr.log"), &restore_output.stderr)?;
    require_success(&restore_output, "restore locked C# mutation projects")?;

    let baseline_output = run_dotnet_baseline(&work)?;
    write_bytes(
        &evidence.join("baseline.stdout.log"),
        &baseline_output.stdout,
    )?;
    write_bytes(
        &evidence.join("baseline.stderr.log"),
        &baseline_output.stderr,
    )?;
    let baseline_passed = baseline_output.status.success();
    if !baseline_passed {
        bail!(
            "unmutated C# baseline failed with {}; raw logs are in {}",
            baseline_output.status,
            evidence.display()
        );
    }

    let config = stryker_config(&shard_sources)?;
    let config_bytes = canonical_json_bytes(&config)?;
    let config_path = work
        .join("app")
        .join("FindMyFiles.Tests")
        .join(".trusted-stryker-shard.json");
    write_bytes(&config_path, &config_bytes)?;
    write_bytes(&evidence.join("trusted-stryker-config.json"), &config_bytes)?;
    policies.push(policy_seal(
        "generated:trusted-stryker-config.json",
        &config_bytes,
    ));
    policies.sort();

    let (tool_executed, process_exit_code, outcomes) = if shard_sources.is_empty() {
        (
            false,
            Some(0),
            CsharpOutcomes {
                killed: Vec::new(),
                invalid: Vec::new(),
                redundant: Vec::new(),
                survived: Vec::new(),
                no_coverage: Vec::new(),
                timeout: Vec::new(),
                ignored: Vec::new(),
                outside_scope_violations: Vec::new(),
            },
        )
    } else {
        let raw = work.join("tool-output").join("csharp");
        reset_owned_directory(&raw, &paths::build_root())?;
        let output = run_stryker(&work, &config_path, &raw)?;
        write_bytes(&evidence.join("stryker.stdout.log"), &output.stdout)?;
        write_bytes(&evidence.join("stryker.stderr.log"), &output.stderr)?;
        let report = find_unique_regular_file(&raw, "mutation-report.json")?;
        let report_bytes = fs::read(&report)
            .with_context(|| format!("read Stryker report {}", report.display()))?;
        write_bytes(&evidence.join("mutation-report.json"), &report_bytes)?;
        let outcomes = parse_csharp_report(
            &evidence.join("mutation-report.json"),
            Some(&work),
            &shard_sources,
        )?;
        (true, output.status.code(), outcomes)
    };
    ensure_hash(
        &config_path,
        &checksum::sha256_hex(&config_bytes),
        "trusted Stryker policy",
    )?;
    validate_csharp_terminal_partition(&outcomes)?;
    let passed = baseline_passed
        && process_exit_code == Some(0)
        && outcomes.survived == expected_survivors
        && outcomes.no_coverage.is_empty()
        && outcomes.timeout.is_empty()
        && outcomes.ignored.is_empty()
        && outcomes.outside_scope_violations.is_empty();

    let mut receipt = CsharpReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        language: "csharp".to_owned(),
        binding: binding(&args),
        tools,
        policies,
        source_inventory_sha256: inventory_hash(&sources)?,
        source_inventory: sources,
        shard_sources_sha256: inventory_hash(&shard_sources)?,
        shard_sources,
        tool_executed,
        baseline_passed,
        process_exit_code,
        outcomes,
        evidence_files: Vec::new(),
        passed,
    };
    receipt.evidence_files = seal_all_evidence_files(&evidence)?;
    mutation::write_json_atomic(&evidence.join("receipt.json"), &receipt)?;
    verify_file_seals(&evidence, &receipt.evidence_files)?;

    if !receipt.passed {
        bail!(
            "C# mutation shard {} failed closed: survived={} (expected accepted={}), no-coverage={}, timeout={}, ignored={}, outside={}, exit={:?}; evidence={}",
            args.shard_index,
            receipt.outcomes.survived.len(),
            expected_survivors.len(),
            receipt.outcomes.no_coverage.len(),
            receipt.outcomes.timeout.len(),
            receipt.outcomes.ignored.len(),
            receipt.outcomes.outside_scope_violations.len(),
            receipt.process_exit_code,
            evidence.display()
        );
    }
    println!(
        "C# mutation shard {}/{} passed: {} valid killed, {} accepted equivalent, {} invalid, {} stock redundant.",
        args.shard_index,
        args.shard_count,
        receipt.outcomes.killed.len(),
        receipt.outcomes.survived.len(),
        receipt.outcomes.invalid.len(),
        receipt.outcomes.redundant.len()
    );
    Ok(())
}

fn run_dotnet_project_restore(work: &Path) -> Result<Output> {
    let mut command = dotnet_command(work);
    command
        .args([
            "restore",
            "FindMyFiles.Tests.csproj",
            "--locked-mode",
            "--runtime",
            "win-x64",
        ])
        .output()
        .context("spawn locked C# project restore")
}

fn build_csharp_native_dependency(work: &Path) -> Result<()> {
    let mut command = rust_command("cargo", work);
    command.args(["build", "--locked", "--release", "-p", "fmf-ffi"]);
    run_status_required(&mut command, "build exact target fmf_engine dependency")
}

fn run_dotnet_baseline(work: &Path) -> Result<Output> {
    let mut command = dotnet_command(work);
    command
        .args([
            "test",
            "FindMyFiles.Tests.csproj",
            "--no-restore",
            "--configuration",
            "Release",
            "--framework",
            CSHARP_TARGET_FRAMEWORK,
            "--runtime",
            "win-x64",
            "--results-directory",
        ])
        .arg(work.join("build").join("mutation-baseline"))
        .args([
            "-p:SkipRustBuild=true",
            "-p:RestoreLockedMode=true",
            "-p:FmfTestSeams=true",
            "-p:FmfArtifactKind=ui-test",
        ])
        .output()
        .context("spawn unmutated C# baseline")
}

fn stryker_config(shard_sources: &[String]) -> Result<Value> {
    let mut patterns = Vec::with_capacity(shard_sources.len());
    for source in shard_sources {
        let relative = source
            .strip_prefix("app/FindMyFiles/")
            .ok_or_else(|| anyhow!("C# source escaped project root: `{source}`"))?;
        if relative
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}' | '!'))
        {
            bail!("C# source path cannot be represented as an exact Stryker glob: `{source}`");
        }
        patterns.push(Value::String(format!("**/{relative}")));
    }
    Ok(serde_json::json!({
        "stryker-config": {
            "project": "FindMyFiles.csproj",
            "test-projects": ["FindMyFiles.Tests.csproj"],
            // Stryker itself recommends at most two sessions on a normal
            // runner. Four made otherwise-killed mutants time out under the
            // hosted Windows CPU/memory contention.
            "concurrency": 2,
            "additional-timeout": 30000,
            "mutate": patterns,
            "mutation-level": "Complete",
            "coverage-analysis": "off",
            "disable-mix-mutants": true,
            "thresholds": {
                "high": 100,
                "low": 100,
                "break": 0
            },
            "report-file-name": "mutation-report",
            "reporters": ["json"],
            "test-runner": "vstest",
            "configuration": "Release",
            "target-framework": CSHARP_TARGET_FRAMEWORK,
            "break-on-initial-test-failure": true
        }
    }))
}

fn reviewed_csharp_survivors(
    policy: &mutation::CsharpReviewedPolicy,
    shard_sources: &[String],
) -> Vec<CsharpMutant> {
    let shard_sources: BTreeSet<&str> = shard_sources.iter().map(String::as_str).collect();
    policy
        .accepted_equivalents
        .iter()
        .filter(|mutant| shard_sources.contains(mutant.path.as_str()))
        .cloned()
        .collect()
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn run_stryker(work: &Path, config: &Path, output: &Path) -> Result<Output> {
    let config_name = config
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("trusted Stryker config filename is not UTF-8"))?;
    let output_arg = path_arg(output)?;
    let mut command = dotnet_command(work);
    command
        .args([
            "tool",
            "run",
            "dotnet-stryker",
            "--",
            "--config-file",
            config_name,
            "--output",
            &output_arg,
            "--skip-version-check",
            "--break-on-initial-test-failure",
        ])
        .output()
        .context("spawn trusted Stryker.NET run")
}

fn find_unique_regular_file(root: &Path, name: &str) -> Result<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut matches = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("walk Stryker output {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || fsx::is_reparse_point(&metadata) {
                bail!(
                    "Stryker output contains a link/reparse point: {}",
                    path.display()
                );
            }
            reject_alternate_streams(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && entry.file_name() == name {
                matches.push(path);
            } else if !metadata.is_file() {
                bail!(
                    "Stryker output contains a non-file entry: {}",
                    path.display()
                );
            }
        }
    }
    if matches.len() != 1 {
        bail!(
            "expected exactly one `{name}` below {}, found {}",
            root.display(),
            matches.len()
        );
    }
    matches
        .pop()
        .ok_or_else(|| anyhow!("unique Stryker report disappeared"))
}

fn parse_csharp_report(
    report_path: &Path,
    expected_work: Option<&Path>,
    shard_sources: &[String],
) -> Result<CsharpOutcomes> {
    let value: Value = mutation::read_json(report_path)?;
    let root = value
        .as_object()
        .ok_or_else(|| anyhow!("{} root is not an object", report_path.display()))?;
    assert_exact_keys(
        root,
        &[
            "files",
            "projectRoot",
            "schemaVersion",
            "testFiles",
            "thresholds",
        ],
        "Stryker report root",
    )?;
    if root.get("schemaVersion").and_then(Value::as_str) != Some("2") {
        bail!("Stryker report schemaVersion is not exact version 2");
    }
    let reported_project = root
        .get("projectRoot")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Stryker report has no projectRoot string"))?;
    let reported_project = normalize_absolute_path_text(reported_project)?;
    if let Some(work) = expected_work {
        let expected_project =
            normalize_absolute_path_text(&work.join("app").join("FindMyFiles").to_string_lossy())?;
        if !text_paths_equal(&reported_project, &expected_project) {
            bail!("Stryker report projectRoot is not the sanitized target project");
        }
    }
    let files = root
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("Stryker report has no files object"))?;
    let selected: BTreeSet<&str> = shard_sources.iter().map(String::as_str).collect();
    let mut seen_selected = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut all_identities = BTreeSet::new();
    let mut killed = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    let mut redundant = BTreeSet::new();
    let mut survived = BTreeSet::new();
    let mut no_coverage = BTreeSet::new();
    let mut timeout = BTreeSet::new();
    let mut ignored = BTreeSet::new();
    let mut outside = BTreeSet::new();

    for (file, value) in files {
        let canonical = canonical_csharp_report_path(&reported_project, file)?;
        let in_scope = selected.contains(canonical.as_str());
        if in_scope {
            seen_selected.insert(canonical.clone());
        }
        let file_object = value
            .as_object()
            .ok_or_else(|| anyhow!("Stryker file `{file}` is not an object"))?;
        assert_exact_keys(
            file_object,
            &["language", "mutants", "source"],
            "Stryker file report",
        )?;
        clean_string(file_object.get("language"), "file.language", false)?;
        arbitrary_string(file_object.get("source"), "file.source", true)?;
        let mutants = file_object
            .get("mutants")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Stryker file `{file}` has no mutants array"))?;
        for mutant_value in mutants {
            let object = mutant_value
                .as_object()
                .ok_or_else(|| anyhow!("Stryker mutant is not an object"))?;
            assert_required_optional_keys(
                object,
                &[
                    "coveredBy",
                    "id",
                    "killedBy",
                    "location",
                    "mutatorName",
                    "replacement",
                    "static",
                    "status",
                ],
                &["statusReason"],
                "Stryker mutant",
            )?;
            let id = clean_string(object.get("id"), "mutant.id", false)?;
            if !ids.insert(id) {
                bail!("Stryker report contains a duplicate mutant id");
            }
            if !object.get("coveredBy").is_some_and(Value::is_array)
                || !object.get("killedBy").is_some_and(Value::is_array)
                || !object.get("static").is_some_and(Value::is_boolean)
            {
                bail!("Stryker mutant coverage/static fields have the wrong types");
            }
            let identity = parse_csharp_mutant(&canonical, object)?;
            if !all_identities.insert(identity.clone()) {
                bail!("Stryker report contains a duplicate canonical mutant identity");
            }
            let status = clean_string(object.get("status"), "mutant.status", false)?;
            let reason = optional_clean_string(object.get("statusReason"), "mutant.statusReason")?;
            let status_identity = CsharpStatus {
                mutant: identity.clone(),
                reason: reason.clone(),
            };
            if !in_scope {
                if !csharp_outside_scope_is_unexecuted(&status, reason.as_deref()) {
                    outside.insert(status_identity);
                }
                continue;
            }
            match status.as_str() {
                "Killed" => {
                    killed.insert(identity);
                }
                "CompileError" if reason.as_deref() == Some("Mutant caused compile errors") => {
                    invalid.insert(status_identity);
                }
                "CompileError" => {
                    bail!("Stryker CompileError has an unknown/missing exact diagnostic");
                }
                "Ignored"
                    if identity.mutator == "Block removal mutation"
                        && reason.as_deref() == Some("Removed by block already covered filter") =>
                {
                    redundant.insert(status_identity);
                }
                "Ignored" => {
                    ignored.insert(status_identity);
                }
                "Survived" => {
                    survived.insert(identity);
                }
                "NoCoverage" => {
                    no_coverage.insert(identity);
                }
                "Timeout" => {
                    timeout.insert(identity);
                }
                other => bail!("Stryker emitted unknown/non-terminal status `{other}`"),
            }
        }
    }
    let expected_selected: BTreeSet<String> =
        shard_sources.iter().cloned().collect::<BTreeSet<_>>();
    if seen_selected != expected_selected {
        bail!("Stryker report omitted one or more exact shard source files");
    }
    Ok(CsharpOutcomes {
        killed: killed.into_iter().collect(),
        invalid: invalid.into_iter().collect(),
        redundant: redundant.into_iter().collect(),
        survived: survived.into_iter().collect(),
        no_coverage: no_coverage.into_iter().collect(),
        timeout: timeout.into_iter().collect(),
        ignored: ignored.into_iter().collect(),
        outside_scope_violations: outside.into_iter().collect(),
    })
}

fn csharp_outside_scope_is_unexecuted(status: &str, reason: Option<&str>) -> bool {
    match status {
        "CompileError" => true,
        "Ignored" => {
            reason.is_some_and(|reason| CSHARP_UNEXECUTED_IGNORE_REASONS.contains(&reason))
        }
        _ => false,
    }
}

fn canonical_csharp_report_path(project_root: &str, file: &str) -> Result<String> {
    if file.is_empty() || file.chars().any(char::is_control) {
        bail!("Stryker source path is empty or contains control characters");
    }
    let normalized_file = file.replace('\\', "/");
    let absolute = if is_absolute_path_text(&normalized_file) {
        normalize_absolute_path_text(&normalized_file)?
    } else {
        let relative = normalize_relative_text(&normalized_file)?;
        format!("{project_root}/{relative}")
    };
    let prefix = format!("{project_root}/");
    let in_project = if cfg!(windows) {
        absolute
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&prefix))
    } else {
        absolute.starts_with(&prefix)
    };
    if !in_project {
        bail!("Stryker source path escaped the sanitized project: `{file}`");
    }
    let relative = &absolute[prefix.len()..];
    let path = format!("app/FindMyFiles/{relative}");
    validate_relative_path(&path)?;
    Ok(path)
}

fn text_paths_equal(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn is_absolute_path_text(path: &str) -> bool {
    path.starts_with('/')
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && path.as_bytes()[2] == b'/')
}

fn normalize_absolute_path_text(path: &str) -> Result<String> {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with("//")
        || normalized.starts_with("//?/")
        || normalized.starts_with("//./")
    {
        bail!("device and UNC report paths are not accepted");
    }
    if !is_absolute_path_text(&normalized) || normalized.chars().any(char::is_control) {
        bail!("report path is not a canonical absolute path: `{path}`");
    }
    let (prefix, remainder) = if let Some(remainder) = normalized.strip_prefix('/') {
        ("/".to_owned(), remainder)
    } else {
        (
            format!(
                "{}:/",
                normalized.as_bytes()[0].to_ascii_uppercase() as char
            ),
            &normalized[3..],
        )
    };
    let relative = normalize_relative_text(remainder)?;
    Ok(format!("{prefix}{relative}"))
}

fn normalize_relative_text(path: &str) -> Result<String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        bail!("report-relative path is empty or not normalized");
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("report path contains parent traversal"),
            value => {
                if value.contains(':')
                    || value.ends_with([' ', '.'])
                    || value.chars().any(char::is_control)
                    || is_windows_reserved_name(value)
                {
                    bail!("report path contains an unsafe component");
                }
                parts.push(value);
            }
        }
    }
    if parts.is_empty() {
        bail!("report-relative path has no components");
    }
    Ok(parts.join("/"))
}

fn parse_csharp_mutant(path: &str, object: &Map<String, Value>) -> Result<CsharpMutant> {
    let mutator = clean_string(object.get("mutatorName"), "mutant.mutatorName", false)?;
    let replacement = arbitrary_string(object.get("replacement"), "mutant.replacement", true)?;
    let location = exact_object(object.get("location"), "mutant.location", &["end", "start"])?;
    let start = exact_object(
        location.get("start"),
        "mutant.location.start",
        &["column", "line"],
    )?;
    let end = exact_object(
        location.get("end"),
        "mutant.location.end",
        &["column", "line"],
    )?;
    let start_line = positive_u64(start.get("line"), "mutant.location.start.line")?;
    let start_column = nonnegative_u64(start.get("column"), "mutant.location.start.column")?;
    let end_line = positive_u64(end.get("line"), "mutant.location.end.line")?;
    let end_column = nonnegative_u64(end.get("column"), "mutant.location.end.column")?;
    if (end_line, end_column) < (start_line, start_column) {
        bail!("Stryker mutant source span ends before it starts");
    }
    Ok(CsharpMutant {
        path: path.to_owned(),
        start_line,
        start_column,
        end_line,
        end_column,
        mutator,
        replacement,
    })
}

fn optional_clean_string(value: Option<&Value>, label: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| anyhow!("{label} is not a string or null"))?;
    if value.chars().any(|character| character == '\0') || value.trim() != value {
        bail!("{label} is not canonical");
    }
    Ok(Some(value.to_owned()))
}

fn nonnegative_u64(value: Option<&Value>, label: &str) -> Result<u64> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{label} is not a non-negative integer"))
}

fn validate_csharp_terminal_partition(outcomes: &CsharpOutcomes) -> Result<()> {
    let mut identities = BTreeSet::new();
    let mut total = 0_usize;
    for mutant in outcomes
        .killed
        .iter()
        .chain(&outcomes.survived)
        .chain(&outcomes.no_coverage)
        .chain(&outcomes.timeout)
    {
        total += 1;
        if !identities.insert(mutant.clone()) {
            bail!("C# receipt terminal sets overlap");
        }
    }
    for status in outcomes
        .invalid
        .iter()
        .chain(&outcomes.redundant)
        .chain(&outcomes.ignored)
    {
        total += 1;
        if !identities.insert(status.mutant.clone()) {
            bail!("C# receipt terminal sets overlap");
        }
    }
    if identities.len() != total {
        bail!("C# receipt terminal sets are not disjoint");
    }
    Ok(())
}

fn inventory_hash<T: Serialize>(values: &[T]) -> Result<String> {
    Ok(checksum::sha256_hex(&serde_json::to_vec(values)?))
}

fn policy_seal(name: impl Into<String>, bytes: &[u8]) -> PolicySeal {
    PolicySeal {
        name: name.into(),
        sha256: checksum::sha256_hex(bytes),
    }
}

fn seal_file(root: &Path, path: &str) -> Result<FileSeal> {
    validate_relative_path(path)?;
    let bytes =
        fs::read(root.join(path)).with_context(|| format!("read evidence file `{path}`"))?;
    Ok(FileSeal {
        path: path.to_owned(),
        size: bytes.len() as u64,
        sha256: checksum::sha256_hex(&bytes),
    })
}

fn seal_all_evidence_files(root: &Path) -> Result<Vec<FileSeal>> {
    let mut paths = Vec::new();
    collect_regular_files(root, root, &mut paths)?;
    paths.retain(|path| path != "receipt.json");
    paths.sort();
    paths.iter().map(|path| seal_file(root, path)).collect()
}

fn collect_regular_files(root: &Path, directory: &Path, output: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read evidence directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || fsx::is_reparse_point(&metadata) {
            bail!("evidence contains a link/reparse point: {}", path.display());
        }
        reject_alternate_streams(&path)?;
        if metadata.is_dir() {
            collect_regular_files(root, &path, output)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .context("evidence file escaped its root")?
                .to_string_lossy()
                .replace('\\', "/");
            validate_relative_path(&relative)?;
            output.push(relative);
        } else {
            bail!("evidence contains a non-file entry: {}", path.display());
        }
    }
    Ok(())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create evidence parent {}", parent.display()))?;
    }
    fsx::write_file_atomic(path, bytes)
        .with_context(|| format!("write evidence {}", path.display()))
}

fn trusted_command(program: impl AsRef<OsStr>) -> Command {
    const PASSTHROUGH: &[&str] = &[
        "APPDATA",
        "COMSPEC",
        "HOMEDRIVE",
        "HOMEPATH",
        "LOCALAPPDATA",
        "NUMBER_OF_PROCESSORS",
        "PATH",
        "PATHEXT",
        "PROCESSOR_ARCHITECTURE",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "RUSTUP_HOME",
        "SystemDrive",
        "SystemRoot",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "WINDIR",
    ];
    let mut command = Command::new(program);
    command.env_clear();
    for name in PASSTHROUGH {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("CI", "true")
        .env("GITHUB_ACTIONS", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

const fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn rust_command(program: &str, work: &Path) -> Command {
    let mut command = trusted_command(program);
    command
        .current_dir(work.join("engine"))
        // cargo-mutants creates `<TEMP>/cargo-mutants-engine-*/crates/...`.
        // Making TEMP the sanitized root preserves the engine/crates ancestry,
        // so the launcher's canonical ../../../app asset path stays valid.
        .env("TEMP", work)
        .env("TMP", work)
        .env("RUSTUP_TOOLCHAIN", RUST_TOOLCHAIN_VERSION)
        .env("CARGO_HOME", work.join(".cargo-home"))
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TERM_COLOR", "never")
        .env("NEXTEST_USER_CONFIG_FILE", "none")
        .env_remove("FMF_BLESS")
        .env("FMF_GOLDEN_DIR", work.join("contract").join("golden"));
    command
}

fn dotnet_command(work: &Path) -> Command {
    let mut command = trusted_command("dotnet");
    command
        .current_dir(work.join("app").join("FindMyFiles.Tests"))
        // The SDK remains exactly pinned and verified above. Testhost targets
        // Microsoft.NETCore.App 10.0.0 and must accept the runner's serviced
        // patch (for example 10.0.10) instead of requiring an insecure RTM copy.
        .env("DOTNET_ROLL_FORWARD", "LatestPatch")
        .env("DOTNET_MULTILEVEL_LOOKUP", "0")
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
        // XAML Compiler dependency resolution is still MAX_PATH-sensitive.
        .env("NUGET_PACKAGES", work.join(".n"))
        .env("RestoreLockedMode", "true")
        .env("SkipRustBuild", "true")
        // Stryker analyses FindMyFiles.csproj standalone and never sees the test
        // project's ProjectReference AdditionalProperties, so without these the
        // mutated assembly ships no InternalsVisibleTo and no FakeEngineClient and
        // discovery finds zero tests. See mutation::CSHARP_TEST_PROFILE.
        .envs(mutation::csharp_test_profile())
        .env("FMF_GOLDEN_DIR", work.join("contract").join("golden"));
    command
}

fn validate_prefixed_version(actual: &str, prefix: &str, label: &str) -> Result<()> {
    if actual == prefix || actual.starts_with(&format!("{prefix} (")) {
        Ok(())
    } else {
        bail!("{label} pin mismatch: expected `{prefix}`, got `{actual}`")
    }
}

struct VerifyContext {
    checkout: Checkout,
    evidence: PathBuf,
}

fn prepare_verifier(args: &VerifyArgs) -> Result<VerifyContext> {
    let controller = canonical_directory(&controller_root(), "controller root")?;
    let target = canonical_directory(&args.target_root, "target root")?;
    let evidence = canonical_directory(&args.evidence_root, "evidence root")?;
    if target == controller
        || target.starts_with(&controller)
        || controller.starts_with(&target)
        || evidence.starts_with(&target)
        || evidence.starts_with(&controller)
    {
        bail!("controller, target, and downloaded evidence roots are not isolated");
    }
    verify_checkout_identity(&controller, &args.controller_sha, "controller")?;
    let checkout = inspect_target_checkout(&target, &args.target_sha)?;
    reject_forbidden_auto_configs(&checkout)?;
    reject_links_and_streams(&evidence)?;
    Ok(VerifyContext { checkout, evidence })
}

fn artifact_directories(root: &Path, language: &str, count: usize) -> Result<Vec<PathBuf>> {
    let expected: BTreeSet<String> = (0..count)
        .map(|index| format!("mutation-{language}-raw-{index}"))
        .collect();
    let mut actual = BTreeMap::new();
    for entry in fs::read_dir(root)
        .with_context(|| format!("enumerate downloaded evidence {}", root.display()))?
    {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("artifact directory name is not UTF-8"))?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || fsx::is_reparse_point(&metadata)
            || !expected.contains(&name)
            || actual.insert(name.clone(), entry.path()).is_some()
        {
            bail!("unexpected, duplicate, or non-directory artifact entry `{name}`");
        }
    }
    if actual.keys().cloned().collect::<BTreeSet<_>>() != expected {
        bail!("downloaded {language} artifact directory set is not exactly all {count} shards");
    }
    (0..count)
        .map(|index| {
            actual
                .remove(&format!("mutation-{language}-raw-{index}"))
                .ok_or_else(|| anyhow!("artifact shard directory disappeared"))
        })
        .collect()
}

fn verify_rust_evidence(args: VerifyArgs) -> Result<()> {
    let context = prepare_verifier(&args)?;
    let sources = source_inventory(&context.checkout, "rust")?;
    let policy_work = paths::build_root()
        .join("mutation")
        .join("verification-policy")
        .join("rust");
    reset_owned_directory(&policy_work, &paths::build_root())?;
    let expected_policies = rust_policies(&context.checkout, &policy_work)?;
    let directories = artifact_directories(&context.evidence, "rust", args.shard_count)?;
    let mut canonical_global: Option<Vec<RustMutant>> = None;
    let mut union = BTreeSet::new();
    let mut killed_total = 0_usize;
    let mut invalid_total = 0_usize;
    for (index, directory) in directories.iter().enumerate() {
        let receipt: RustReceipt = mutation::read_json(&directory.join("receipt.json"))?;
        validate_rust_receipt_header(&receipt, &args, index, &expected_policies, &sources)?;
        validate_rust_allowed_files(directory, &receipt)?;
        verify_file_seals(directory, &receipt.evidence_files)?;

        let global = parse_rust_mutant_list(&directory.join("global-mutants.json"), true)?;
        require_generated_sources_in_inventory(&global, &sources, "Rust")?;
        if let Some(expected) = &canonical_global {
            if &global != expected {
                bail!("Rust global mutant inventory differs across shards");
            }
        } else {
            canonical_global = Some(global.clone());
        }
        if receipt.global_mutant_count != global.len()
            || receipt.global_mutants_sha256 != inventory_hash(&global)?
        {
            bail!("Rust shard {index} global inventory seal is false");
        }

        let shard = parse_rust_mutant_list(&directory.join("mutants.json"), false)?;
        let shard_set: BTreeSet<RustMutant> = shard.iter().cloned().collect();
        if shard_set.len() != shard.len()
            || receipt.shard_mutant_count != shard.len()
            || receipt.shard_mutants_sha256 != inventory_hash(&shard)?
        {
            bail!("Rust shard {index} mutant inventory seal is false or duplicated");
        }
        let expected_shard: BTreeSet<RustMutant> = global
            .iter()
            .enumerate()
            .filter(|(position, _)| position % args.shard_count == index)
            .map(|(_, mutant)| mutant.clone())
            .collect();
        if shard_set != expected_shard {
            bail!("Rust shard {index} is not its exact round-robin partition");
        }
        for mutant in &shard_set {
            if !union.insert(mutant.clone()) {
                bail!("Rust shard evidence overlaps on mutant `{}`", mutant.name);
            }
        }

        let parsed = parse_rust_outcomes(directory, &shard_set)?;
        let derived_invalid =
            verify_rust_invalid_diagnostics(directory, &parsed.invalid, &receipt)?;
        let derived = RustOutcomes {
            killed: parsed.killed,
            invalid: derived_invalid,
            survived: parsed.survived,
            timeout: parsed.timeout,
        };
        if derived != receipt.outcomes
            || !parsed.baseline_passed
            || !receipt.baseline_passed
            || receipt.process_exit_code != Some(0)
            || !receipt.passed
            || !receipt.outcomes.survived.is_empty()
            || !receipt.outcomes.timeout.is_empty()
        {
            bail!("Rust shard {index} receipt/report gate semantics do not pass");
        }
        let baseline = fs::read(directory.join("baseline.log"))?;
        if baseline.is_empty() {
            bail!("Rust shard {index} baseline diagnostic is empty");
        }
        validate_rust_terminal_partition(&shard_set, &receipt.outcomes)?;
        killed_total += receipt.outcomes.killed.len();
        invalid_total += receipt.outcomes.invalid.len();
    }
    let global = canonical_global.ok_or_else(|| anyhow!("no Rust global inventory"))?;
    if union != global.iter().cloned().collect() {
        bail!("Rust shard union is not exhaustive over the global mutant inventory");
    }
    write_verified_summary(
        "rust",
        &args,
        &sources,
        VerifiedCounts {
            mutants: global.len(),
            killed: killed_total,
            invalid: invalid_total,
            redundant: 0,
            accepted: 0,
        },
    )?;
    println!(
        "Independently verified all {} Rust mutation shards: {} valid killed, {} invalid.",
        args.shard_count, killed_total, invalid_total
    );
    Ok(())
}

fn validate_rust_receipt_header(
    receipt: &RustReceipt,
    args: &VerifyArgs,
    index: usize,
    policies: &[PolicySeal],
    sources: &[SourceFile],
) -> Result<()> {
    validate_receipt_common(
        receipt.schema_version,
        &receipt.language,
        &receipt.binding,
        "rust",
        args,
        index,
    )?;
    validate_rust_tool_receipt(&receipt.tools)?;
    if receipt.policies != policies
        || receipt.source_inventory != sources
        || receipt.source_inventory_sha256 != inventory_hash(sources)?
    {
        bail!("Rust shard {index} policy or source inventory is not controller-derived");
    }
    Ok(())
}

fn validate_rust_tool_receipt(tools: &RustTools) -> Result<()> {
    validate_prefixed_version(
        &first_line(&tools.rustc)?,
        &format!("rustc {RUST_TOOLCHAIN_VERSION}"),
        "receipt rustc",
    )?;
    validate_prefixed_version(
        &first_line(&tools.cargo)?,
        &format!("cargo {RUST_TOOLCHAIN_VERSION}"),
        "receipt cargo",
    )?;
    if first_line(&tools.cargo_mutants)? != format!("cargo-mutants {CARGO_MUTANTS_VERSION}")
        || !first_line(&tools.cargo_nextest)?
            .starts_with(&format!("cargo-nextest {CARGO_NEXTEST_VERSION}"))
    {
        bail!("Rust receipt tool identities do not match controller pins");
    }
    Ok(())
}

fn validate_rust_allowed_files(root: &Path, receipt: &RustReceipt) -> Result<()> {
    let mut expected: BTreeSet<String> = RUST_REPORT_FILES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    expected.insert("baseline.log".to_owned());
    expected.insert("global-mutants.json".to_owned());
    for invalid in &receipt.outcomes.invalid {
        if invalid.reason != "Unviable" {
            bail!("Rust invalid outcome reason is not exact `Unviable`");
        }
        let identity_hash = checksum::sha256_hex(&serde_json::to_vec(&invalid.mutant)?);
        let expected_path = format!("invalid/{identity_hash}.log");
        if invalid.diagnostic.path != expected_path {
            bail!("Rust invalid diagnostic path is not identity-derived");
        }
        expected.insert(expected_path);
    }
    validate_allowed_files(root, &receipt.evidence_files, &expected)
}

fn verify_rust_invalid_diagnostics(
    root: &Path,
    parsed: &[(RustMutant, String)],
    receipt: &RustReceipt,
) -> Result<Vec<RustInvalid>> {
    let parsed: BTreeSet<RustMutant> = parsed.iter().map(|(mutant, _)| mutant.clone()).collect();
    let receipt_set: BTreeSet<RustMutant> = receipt
        .outcomes
        .invalid
        .iter()
        .map(|invalid| invalid.mutant.clone())
        .collect();
    if parsed != receipt_set || parsed.len() != receipt.outcomes.invalid.len() {
        bail!("Rust Unviable inventory is not 1:1 between report and receipt");
    }
    for invalid in &receipt.outcomes.invalid {
        let bytes = fs::read(root.join(&invalid.diagnostic.path))?;
        if bytes.is_empty() || seal_file(root, &invalid.diagnostic.path)? != invalid.diagnostic {
            bail!(
                "Rust Unviable diagnostic is empty or unsealed for `{}`",
                invalid.mutant.name
            );
        }
    }
    Ok(receipt.outcomes.invalid.clone())
}

fn verify_csharp_evidence(args: VerifyArgs) -> Result<()> {
    let context = prepare_verifier(&args)?;
    let reviewed_policy = mutation::read_csharp_reviewed_policy(&controller_root())?;
    let sources = source_inventory(&context.checkout, "csharp")?;
    let policy_work = paths::build_root()
        .join("mutation")
        .join("verification-policy")
        .join("csharp");
    reset_owned_directory(&policy_work, &paths::build_root())?;
    let base_policies = csharp_policies(&context.checkout, &policy_work)?;
    let directories = artifact_directories(&context.evidence, "csharp", args.shard_count)?;
    let mut source_union = BTreeSet::new();
    let mut mutant_union = BTreeSet::new();
    let mut killed_total = 0_usize;
    let mut invalid_total = 0_usize;
    let mut redundant_total = 0_usize;
    let mut accepted_total = 0_usize;
    for (index, directory) in directories.iter().enumerate() {
        let receipt: CsharpReceipt = mutation::read_json(&directory.join("receipt.json"))?;
        let expected_sources: Vec<String> = sources
            .iter()
            .enumerate()
            .filter(|(position, _)| position % args.shard_count == index)
            .map(|(_, source)| source.path.clone())
            .collect();
        let expected_survivors = reviewed_csharp_survivors(&reviewed_policy, &expected_sources);
        let config = stryker_config(&expected_sources)?;
        let config_bytes = canonical_json_bytes(&config)?;
        let mut expected_policies = base_policies.clone();
        expected_policies.push(policy_seal(
            "generated:trusted-stryker-config.json",
            &config_bytes,
        ));
        expected_policies.sort();
        validate_csharp_receipt_header(
            &receipt,
            &args,
            index,
            &expected_policies,
            &sources,
            &expected_sources,
        )?;
        validate_csharp_allowed_files(directory, &receipt)?;
        verify_file_seals(directory, &receipt.evidence_files)?;
        let recorded_config = fs::read(directory.join("trusted-stryker-config.json"))?;
        if recorded_config != config_bytes {
            bail!("C# shard {index} trusted Stryker config is not controller-generated");
        }
        for source in &receipt.shard_sources {
            if !source_union.insert(source.clone()) {
                bail!("C# source partition overlaps at `{source}`");
            }
        }

        let derived = if receipt.tool_executed {
            parse_csharp_report(
                &directory.join("mutation-report.json"),
                None,
                &expected_sources,
            )?
        } else {
            if !expected_sources.is_empty() {
                bail!("C# shard {index} skipped Stryker despite a non-empty source partition");
            }
            CsharpOutcomes {
                killed: Vec::new(),
                invalid: Vec::new(),
                redundant: Vec::new(),
                survived: Vec::new(),
                no_coverage: Vec::new(),
                timeout: Vec::new(),
                ignored: Vec::new(),
                outside_scope_violations: Vec::new(),
            }
        };
        if derived != receipt.outcomes
            || !receipt.baseline_passed
            || receipt.process_exit_code != Some(0)
            || !receipt.passed
            || receipt.outcomes.survived != expected_survivors
            || !receipt.outcomes.no_coverage.is_empty()
            || !receipt.outcomes.timeout.is_empty()
            || !receipt.outcomes.ignored.is_empty()
            || !receipt.outcomes.outside_scope_violations.is_empty()
        {
            bail!("C# shard {index} receipt/report gate semantics do not pass");
        }
        validate_csharp_terminal_partition(&receipt.outcomes)?;
        for mutant in receipt
            .outcomes
            .killed
            .iter()
            .chain(&receipt.outcomes.survived)
            .chain(receipt.outcomes.invalid.iter().map(|entry| &entry.mutant))
            .chain(receipt.outcomes.redundant.iter().map(|entry| &entry.mutant))
        {
            if !mutant_union.insert(mutant.clone()) {
                bail!("C# mutant evidence overlaps across source shards");
            }
        }
        killed_total += receipt.outcomes.killed.len();
        accepted_total += receipt.outcomes.survived.len();
        invalid_total += receipt.outcomes.invalid.len();
        redundant_total += receipt.outcomes.redundant.len();
    }
    let expected_source_union: BTreeSet<String> =
        sources.iter().map(|source| source.path.clone()).collect();
    if source_union != expected_source_union || mutant_union.is_empty() {
        bail!("C# shard union is not exhaustive or generated no mutants");
    }
    write_verified_summary(
        "csharp",
        &args,
        &sources,
        VerifiedCounts {
            mutants: mutant_union.len(),
            killed: killed_total,
            invalid: invalid_total,
            redundant: redundant_total,
            accepted: accepted_total,
        },
    )?;
    println!(
        "Independently verified all {} C# mutation shards: {} valid killed, {} accepted equivalent, {} invalid, {} stock redundant.",
        args.shard_count, killed_total, accepted_total, invalid_total, redundant_total
    );
    Ok(())
}

fn validate_csharp_receipt_header(
    receipt: &CsharpReceipt,
    args: &VerifyArgs,
    index: usize,
    policies: &[PolicySeal],
    sources: &[SourceFile],
    shard_sources: &[String],
) -> Result<()> {
    validate_receipt_common(
        receipt.schema_version,
        &receipt.language,
        &receipt.binding,
        "csharp",
        args,
        index,
    )?;
    validate_prefixed_version(
        &receipt.tools.rustc,
        &format!("rustc {RUST_TOOLCHAIN_VERSION}"),
        "C# receipt rustc",
    )?;
    validate_prefixed_version(
        &receipt.tools.cargo,
        &format!("cargo {RUST_TOOLCHAIN_VERSION}"),
        "C# receipt cargo",
    )?;
    if receipt.tools.dotnet_sdk != DOTNET_SDK_VERSION
        || receipt.tools.dotnet_stryker != format!("dotnet-stryker {STRYKER_VERSION}")
        || receipt.policies != policies
        || receipt.source_inventory != sources
        || receipt.source_inventory_sha256 != inventory_hash(sources)?
        || receipt.shard_sources != shard_sources
        || receipt.shard_sources_sha256 != inventory_hash(shard_sources)?
        || receipt.tool_executed == shard_sources.is_empty()
    {
        bail!("C# shard {index} identity, policy, tools, or source partition is false");
    }
    Ok(())
}

fn validate_csharp_allowed_files(root: &Path, receipt: &CsharpReceipt) -> Result<()> {
    let mut expected = BTreeSet::from([
        "baseline.stderr.log".to_owned(),
        "baseline.stdout.log".to_owned(),
        "restore.stderr.log".to_owned(),
        "restore.stdout.log".to_owned(),
        "trusted-stryker-config.json".to_owned(),
    ]);
    if receipt.tool_executed {
        expected.extend([
            "mutation-report.json".to_owned(),
            "stryker.stderr.log".to_owned(),
            "stryker.stdout.log".to_owned(),
        ]);
    }
    validate_allowed_files(root, &receipt.evidence_files, &expected)
}

fn validate_allowed_files(
    root: &Path,
    seals: &[FileSeal],
    expected: &BTreeSet<String>,
) -> Result<()> {
    let sealed: BTreeSet<String> = seals.iter().map(|seal| seal.path.clone()).collect();
    if sealed.len() != seals.len() || &sealed != expected {
        bail!("artifact contains a receipt-approved but policy-unknown evidence file");
    }
    let mut actual = Vec::new();
    collect_regular_files(root, root, &mut actual)?;
    let actual: BTreeSet<String> = actual.into_iter().collect();
    let mut expected_with_receipt = expected.clone();
    expected_with_receipt.insert("receipt.json".to_owned());
    if actual != expected_with_receipt {
        bail!("artifact file set is not exact");
    }
    Ok(())
}

fn validate_receipt_common(
    schema: u32,
    language: &str,
    binding: &RunBinding,
    expected_language: &str,
    args: &VerifyArgs,
    index: usize,
) -> Result<()> {
    let expected = RunBinding {
        controller_sha: args.controller_sha.clone(),
        target_sha: args.target_sha.clone(),
        run_id: args.run_id,
        run_attempt: args.run_attempt,
        shard_index: index,
        shard_count: args.shard_count,
    };
    if schema != RECEIPT_SCHEMA_VERSION || language != expected_language || binding != &expected {
        bail!(
            "{expected_language} shard {index} receipt is bound to another schema/run/attempt/commit/shard"
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VerifiedCounts {
    mutants: usize,
    killed: usize,
    invalid: usize,
    redundant: usize,
    accepted: usize,
}

fn write_verified_summary(
    language: &str,
    args: &VerifyArgs,
    sources: &[SourceFile],
    counts: VerifiedCounts,
) -> Result<()> {
    let output = paths::build_root().join("mutation").join("verified");
    fs::create_dir_all(&output)
        .with_context(|| format!("create verified mutation output {}", output.display()))?;
    let summary = serde_json::json!({
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "language": language,
        "controller_sha": args.controller_sha,
        "target_sha": args.target_sha,
        "run_id": args.run_id,
        "run_attempt": args.run_attempt,
        "shard_count": args.shard_count,
        "source_inventory_sha256": inventory_hash(sources)?,
        "source_file_count": sources.len(),
        "mutant_count": counts.mutants,
        "valid_killed": counts.killed,
        "accepted_equivalent": counts.accepted,
        "invalid": counts.invalid,
        "stock_redundant": counts.redundant,
        "passed": true
    });
    mutation::write_json_atomic(&output.join(format!("{language}.json")), &summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> RunArgs {
        RunArgs {
            target_root: Some(PathBuf::from("target")),
            target_sha: Some("a".repeat(40)),
            controller_sha: Some("b".repeat(40)),
            run_id: Some(1),
            run_attempt: Some(1),
            shard_index: Some(0),
            shard_count: Some(16),
        }
    }

    #[test]
    fn local_mode_requires_every_ci_identity_field_to_be_absent() {
        assert!(RunArgs::default().into_ci().unwrap().is_none());
        let mut partial = complete();
        partial.run_attempt = None;
        assert!(partial.into_ci().is_err());
    }

    #[test]
    fn ci_identity_is_lowercase_exact_and_sixteen_way() {
        let args = complete().into_ci().unwrap().unwrap();
        assert_eq!(args.shard_index, 0);

        let mut uppercase = complete();
        uppercase.target_sha = Some("A".repeat(40));
        assert!(uppercase.into_ci().is_err());

        let mut wrong_count = complete();
        wrong_count.shard_count = Some(15);
        assert!(wrong_count.into_ci().is_err());
    }

    #[test]
    fn only_controller_owned_cargo_configs_are_trusted() {
        assert!(is_trusted_auto_config("engine/.cargo/config.toml"));
        assert!(is_trusted_auto_config("xtask/.cargo/config.toml"));
        assert!(!is_trusted_auto_config(".cargo/config.toml"));
        assert!(!is_trusted_auto_config("app/.cargo/config.toml"));
        assert!(is_forbidden_auto_config("xtask/.cargo/config.toml"));
    }

    #[test]
    fn rust_copy_scope_includes_only_required_cross_tree_inputs() {
        assert!(copy_path_for_language(
            "app/FindMyFiles/Engine/Generated/EngineContract.g.cs",
            "rust"
        ));
        assert!(copy_path_for_language(
            "app/FindMyFiles/Assets/find-my-files.ico",
            "rust"
        ));
        assert!(!copy_path_for_language(
            "app/FindMyFiles/Views/MainPage.xaml",
            "rust"
        ));
    }

    #[test]
    fn mutation_work_paths_are_short_and_language_scoped() {
        let args = complete().into_ci().unwrap().unwrap();
        let rust = mutation_work_dir(&args, "rust").unwrap();
        let csharp = mutation_work_dir(&args, "csharp").unwrap();
        assert_eq!(
            rust.strip_prefix(paths::build_root()).unwrap(),
            Path::new("mw").join("r").join("1-1-0")
        );
        assert_eq!(
            csharp.strip_prefix(paths::build_root()).unwrap(),
            Path::new("mw").join("c").join("1-1-0")
        );
        assert!(mutation_work_dir(&args, "python").is_err());
    }

    #[test]
    fn tool_caches_stay_short_and_rust_temp_preserves_asset_ancestry() {
        let work = PathBuf::from(r"C:\repo\build\mw\r\1-1-0");
        let rust = rust_command("cargo", &work);
        let rust_env: BTreeMap<_, _> = rust
            .get_envs()
            .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
            .collect();
        assert_eq!(
            rust_env.get(OsStr::new("TEMP")),
            Some(&work.as_os_str().to_owned())
        );
        assert_eq!(
            rust_env.get(OsStr::new("TMP")),
            Some(&work.as_os_str().to_owned())
        );
        let mutant_crate = work
            .join("cargo-mutants-engine.tmp")
            .join("crates")
            .join("fmf-launcher");
        assert_eq!(mutant_crate.ancestors().nth(3), Some(work.as_path()));

        let csharp = dotnet_command(&work);
        let csharp_env: BTreeMap<_, _> = csharp
            .get_envs()
            .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
            .collect();
        assert_eq!(
            csharp_env.get(OsStr::new("NUGET_PACKAGES")),
            Some(&work.join(".n").into_os_string())
        );
        assert_eq!(
            csharp_env.get(OsStr::new("DOTNET_ROLL_FORWARD")),
            Some(&OsStr::new("LatestPatch").to_owned())
        );
    }

    #[test]
    fn rust_nextest_locked_is_forwarded_only_by_cargo_mutants() {
        assert!(!RUST_MUTATION_NEXTEST_ARGS.contains(&"--locked"));
    }

    #[test]
    fn optional_json_keys_allow_absence_but_never_schema_drift() {
        let mut object = Map::new();
        object.insert("id".to_owned(), Value::Null);
        object.insert("status".to_owned(), Value::Null);
        assert!(assert_required_optional_keys(
            &object,
            &["id", "status"],
            &["statusReason"],
            "fixture",
        )
        .is_ok());

        object.insert("statusReason".to_owned(), Value::Null);
        assert!(assert_required_optional_keys(
            &object,
            &["id", "status"],
            &["statusReason"],
            "fixture",
        )
        .is_ok());

        object.insert("unexpected".to_owned(), Value::Null);
        assert!(assert_required_optional_keys(
            &object,
            &["id", "status"],
            &["statusReason"],
            "fixture",
        )
        .is_err());
        object.remove("unexpected");
        object.remove("id");
        assert!(assert_required_optional_keys(
            &object,
            &["id", "status"],
            &["statusReason"],
            "fixture",
        )
        .is_err());
    }

    #[test]
    fn csharp_outside_scope_accepts_only_outcomes_that_never_executed() {
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
                csharp_outside_scope_is_unexecuted(status, reason),
                "unexecuted outside-scope outcome was rejected: {status} {reason:?}"
            );
        }
        for (status, reason) in [
            ("Killed", None),
            ("Survived", None),
            ("NoCoverage", None),
            ("Timeout", None),
            ("Ignored", None),
            ("Ignored", Some("Removed by block already covered filter")),
        ] {
            assert!(
                !csharp_outside_scope_is_unexecuted(status, reason),
                "executed or unexplained outside-scope outcome was accepted: {status} {reason:?}"
            );
        }
    }

    #[test]
    fn csharp_reviewed_survivors_follow_the_exact_source_partition() {
        let accepted = |path: &str, line| CsharpMutant {
            path: path.to_owned(),
            start_line: line,
            start_column: 1,
            end_line: line,
            end_column: 2,
            mutator: "Boolean mutation".to_owned(),
            replacement: "true".to_owned(),
        };
        let first = accepted("app/FindMyFiles/Engine/A.cs", 1);
        let second = accepted("app/FindMyFiles/Engine/B.cs", 2);
        let policy = mutation::CsharpReviewedPolicy {
            examined_files: vec![first.path.clone(), second.path.clone()],
            accepted_equivalents: vec![first, second.clone()],
        };

        assert_eq!(
            reviewed_csharp_survivors(&policy, std::slice::from_ref(&second.path)),
            vec![second]
        );
    }

    #[test]
    fn failed_rust_baseline_remains_parseable_for_diagnostic_copy() {
        assert!(parse_rust_baseline_summary("Success").unwrap());
        assert!(!parse_rust_baseline_summary("Failure").unwrap());
        assert!(parse_rust_baseline_summary("Timeout").is_err());

        let mutant = RustMutant {
            name: "fixture".to_owned(),
            package: "fmf-core".to_owned(),
            path: "crates/fmf-core/src/lib.rs".to_owned(),
            line: 1,
            column: 1,
            mutation: "fixture mutation".to_owned(),
        };
        let generated = BTreeSet::from([mutant.clone()]);
        let empty_set = BTreeSet::new();
        let empty_map = BTreeMap::new();
        assert!(validate_parsed_rust_partition(
            false, &generated, &empty_set, &empty_map, &empty_set, &empty_set,
        )
        .is_ok());
        assert!(validate_parsed_rust_partition(
            true,
            &generated,
            &BTreeSet::from([mutant.clone()]),
            &empty_map,
            &empty_set,
            &empty_set,
        )
        .is_ok());
        assert!(validate_parsed_rust_partition(
            false,
            &generated,
            &BTreeSet::from([mutant]),
            &empty_map,
            &empty_set,
            &empty_set,
        )
        .is_err());
    }
}
