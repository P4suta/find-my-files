use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};
use std::process::{Child, Command};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::{checksum::sha256_hex, cmd, paths};

const SCHEMA_VERSION: u32 = 1;
const MAX_JOBS: usize = 2;
const MAX_TIMEOUT_SECONDS: u32 = 600;
const MAX_ADDRESS_SPACE_BYTES: u64 = 8_589_934_592;
const MANIFEST_PATH: &str = "verification/proofs.json";
const RECEIPT_NAME: &str = "proofs.json";
const MIN_COVERS: usize = 2;
const QUALIFIED_ATTRIBUTE_SEGMENTS: usize = 2;
const MAX_SHARDS: usize = 4;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    schema_version: u32,
    kani_version: String,
    jobs: usize,
    timeout_seconds: u32,
    address_space_bytes: u64,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    package: Package,
    source: String,
    prefix: String,
    harnesses: Vec<String>,
}

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
enum Package {
    FmfContract,
    FmfCore,
    FmfRuntime,
    Xtask,
}

impl Package {
    const fn name(self) -> &'static str {
        match self {
            Self::FmfContract => "fmf-contract",
            Self::FmfCore => "fmf-core",
            Self::FmfRuntime => "fmf-runtime",
            Self::Xtask => "xtask",
        }
    }

    const fn manifest(self) -> &'static str {
        match self {
            Self::FmfContract | Self::FmfCore | Self::FmfRuntime => "engine/Cargo.toml",
            Self::Xtask => "xtask/Cargo.toml",
        }
    }

    const fn selector(self) -> &'static [&'static str] {
        match self {
            Self::FmfContract | Self::FmfCore | Self::FmfRuntime => &["--lib"],
            Self::Xtask => &["--bin", "xtask"],
        }
    }

    fn source_root(self) -> String {
        match self {
            Self::FmfContract | Self::FmfCore | Self::FmfRuntime => {
                format!("engine/crates/{}/src/", self.name())
            }
            Self::Xtask => "xtask/src/".into(),
        }
    }
}

#[derive(Default)]
struct BodyChecks {
    assertions: usize,
    covers: usize,
}

impl<'ast> Visit<'ast> for BodyChecks {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let name = node
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        match name.as_deref() {
            Some("assert" | "assert_eq" | "assert_ne") => self.assertions += 1,
            Some("cover")
                if node
                    .path
                    .segments
                    .first()
                    .is_some_and(|s| s.ident == "kani") =>
            {
                self.covers += 1;
            }
            _ => {}
        }
        syn::visit::visit_macro(self, node);
    }
}

fn kani_attribute(attribute: &syn::Attribute, name: &str) -> bool {
    let segments = &attribute.path().segments;
    segments.len() == QUALIFIED_ATTRIBUTE_SEGMENTS
        && segments[0].ident == "kani"
        && segments[1].ident == name
}

fn declarations(source: &str) -> Result<BTreeSet<String>> {
    let parsed = syn::parse_file(source).context("parse proof source")?;
    let mut names = BTreeSet::new();
    for item in parsed.items {
        let syn::Item::Fn(function) = item else {
            continue;
        };
        if !function
            .attrs
            .iter()
            .any(|attribute| kani_attribute(attribute, "proof"))
        {
            continue;
        }
        ensure!(
            function.sig.inputs.is_empty(),
            "proof must generate its own inputs"
        );
        let unwind = function
            .attrs
            .iter()
            .find(|attribute| kani_attribute(attribute, "unwind"))
            .context("proof lacks an explicit unwind bound")?;
        let bound: syn::LitInt = unwind
            .parse_args()
            .context("unwind bound must be an integer")?;
        ensure!(
            bound.base10_parse::<usize>()? > 0,
            "unwind bound must be positive"
        );
        let mut body = BodyChecks::default();
        body.visit_block(&function.block);
        ensure!(
            body.assertions > 0 && body.covers >= MIN_COVERS,
            "proof {} needs assertions and at least {MIN_COVERS} reachability checks",
            function.sig.ident
        );
        ensure!(
            names.insert(function.sig.ident.to_string()),
            "duplicate proof function"
        );
    }
    Ok(names)
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !path.contains('\\')
}

fn read_inventory(root: &Path) -> Result<Inventory> {
    let inventory: Inventory = serde_json::from_slice(&fs::read(root.join(MANIFEST_PATH))?)?;
    validate_inventory(&inventory)?;
    let pins = fs::read_to_string(root.join("mise.toml"))?.parse::<toml_edit::DocumentMut>()?;
    ensure!(
        pins["tools"]["cargo:kani-verifier"]["version"].as_str() == Some(&inventory.kani_version),
        "Kani inventory and mise pins disagree"
    );
    for target in &inventory.targets {
        let source = fs::read_to_string(root.join(&target.source))?;
        let expected: BTreeSet<_> = target.harnesses.iter().cloned().collect();
        ensure!(
            declarations(&source)? == expected,
            "proof inventory drift in {}",
            target.source
        );
    }
    Ok(inventory)
}

fn validate_inventory(inventory: &Inventory) -> Result<()> {
    ensure!(
        inventory.schema_version == SCHEMA_VERSION,
        "unsupported proof inventory version"
    );
    ensure!(
        (1..=MAX_JOBS).contains(&inventory.jobs),
        "invalid proof job budget"
    );
    ensure!(
        (1..=MAX_TIMEOUT_SECONDS).contains(&inventory.timeout_seconds),
        "invalid proof timeout"
    );
    ensure!(
        address_space_is_bounded(inventory.address_space_bytes),
        "invalid proof process address-space budget"
    );
    ensure!(
        !inventory.kani_version.is_empty() && !inventory.targets.is_empty(),
        "empty proof inventory"
    );
    let mut names = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for target in &inventory.targets {
        ensure!(
            relative(&target.source)
                && target.source.starts_with(&target.package.source_root())
                && target.source.ends_with("_proofs.rs"),
            "invalid proof source"
        );
        ensure!(sources.insert(&target.source), "duplicate proof source");
        ensure!(
            !target.prefix.is_empty()
                && target.prefix.ends_with("::")
                && !target.harnesses.is_empty(),
            "empty proof target"
        );
        for name in &target.harnesses {
            ensure!(
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "invalid harness name"
            );
            ensure!(
                names.insert((&target.package, format!("{}{name}", target.prefix))),
                "duplicate harness"
            );
        }
    }
    Ok(())
}

const fn address_space_is_bounded(bytes: u64) -> bool {
    bytes > 0 && bytes <= MAX_ADDRESS_SPACE_BYTES
}

fn expected(inventory: &Inventory, package: Package) -> BTreeSet<String> {
    inventory
        .targets
        .iter()
        .filter(|target| target.package == package)
        .flat_map(|target| {
            target
                .harnesses
                .iter()
                .map(|name| format!("{}{name}", target.prefix))
        })
        .collect()
}

#[derive(Deserialize)]
struct KaniReport {
    metadata: Metadata,
    verification_results: Verification,
    property_details: Vec<HarnessProperties>,
}

#[derive(Deserialize)]
struct Metadata {
    kani_version: String,
}

#[derive(Deserialize)]
struct Verification {
    summary: Summary,
    results: Vec<HarnessResult>,
}

#[derive(Deserialize)]
struct Summary {
    total_harnesses: usize,
    executed: usize,
    successful: usize,
    failed: usize,
}

impl Summary {
    const fn complete(&self, expected: usize) -> bool {
        expected > 0
            && self.total_harnesses == expected
            && self.executed == expected
            && self.successful == expected
            && self.failed == 0
    }
}

#[cfg(kani)]
#[path = "proof_proofs.rs"]
mod kani_proofs;

#[derive(Deserialize)]
struct HarnessResult {
    harness_id: String,
    status: String,
    checks: Vec<Check>,
}

#[derive(Deserialize)]
struct Check {
    status: String,
    category: String,
}

#[derive(Deserialize)]
struct HarnessProperties {
    harness_id: String,
    property_details: CheckCounts,
}

#[derive(Deserialize)]
struct CheckCounts {
    total_properties: usize,
    passed: usize,
    failed: usize,
    undetermined: usize,
    solver_error: usize,
    satisfied: usize,
    unsatisfiable: usize,
    uncovered: usize,
}

fn validate_report(report: &KaniReport, names: &BTreeSet<String>, version: &str) -> Result<()> {
    ensure!(!names.is_empty(), "no required harnesses");
    ensure!(
        report.metadata.kani_version == version,
        "unexpected Kani version"
    );
    let summary = &report.verification_results.summary;
    ensure!(
        summary.complete(names.len()),
        "incomplete or failed proof run"
    );
    let results = &report.verification_results.results;
    ensure!(
        results.len() == names.len()
            && results
                .iter()
                .map(|r| r.harness_id.clone())
                .collect::<BTreeSet<_>>()
                == *names,
        "executed harness inventory mismatch"
    );
    for result in results {
        ensure!(
            result.status == "Success" && !result.checks.is_empty(),
            "failed or empty proof {}",
            result.harness_id
        );
        ensure!(
            result.checks.iter().all(|check| matches!(
                check.status.as_str(),
                "Success" | "Satisfied" | "Unreachable"
            )),
            "failed or unknown property status"
        );
        ensure!(
            result
                .checks
                .iter()
                .any(|check| check.status == "Success" && check.category == "assertion"),
            "no reachable assertion in {}",
            result.harness_id
        );
    }
    ensure!(
        report.property_details.len() == names.len()
            && report
                .property_details
                .iter()
                .map(|r| r.harness_id.clone())
                .collect::<BTreeSet<_>>()
                == *names,
        "property inventory mismatch"
    );
    for harness in &report.property_details {
        let p = &harness.property_details;
        ensure!(
            p.total_properties > 0
                && p.passed > 0
                && p.satisfied >= MIN_COVERS
                && p.failed == 0
                && p.undetermined == 0
                && p.solver_error == 0
                && p.unsatisfiable == 0
                && p.uncovered == 0,
            "unproved or vacuous harness {}",
            harness.harness_id
        );
    }
    Ok(())
}

fn source_files(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "proof inputs must not contain symlinks");
        let path = entry.path();
        if kind.is_dir() {
            source_files(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative, sha256_hex(&fs::read(&path)?));
        }
    }
    Ok(())
}

fn fingerprint(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for path in [
        "engine/Cargo.toml",
        "engine/Cargo.lock",
        "mise.toml",
        MANIFEST_PATH,
        "engine/.cargo/config.toml",
        "xtask/Cargo.toml",
        "xtask/Cargo.lock",
    ] {
        files.insert(path.to_owned(), sha256_hex(&fs::read(root.join(path))?));
    }
    source_files(root, &root.join("xtask/src"), &mut files)?;
    for package in ["fmf-contract", "fmf-core", "fmf-runtime", "fmf-proto"] {
        let package_dir = root.join("engine/crates").join(package);
        for filename in ["Cargo.toml", "build.rs"] {
            let path = package_dir.join(filename);
            if path.exists() {
                files.insert(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    sha256_hex(&fs::read(path)?),
                );
            }
        }
        source_files(root, &package_dir.join("src"), &mut files)?;
    }
    Ok(files)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    source: BTreeMap<String, String>,
    results: BTreeMap<String, String>,
    #[serde(default)]
    scope: Scope,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Scope {
    #[default]
    Complete,
    Shard {
        index: usize,
        count: usize,
    },
    Collection {
        count: usize,
    },
}

#[derive(Clone, Copy)]
struct Shard {
    index: usize,
    count: usize,
}

impl Shard {
    fn new(index: usize, count: usize) -> Result<Self> {
        ensure!(
            (1..=MAX_SHARDS).contains(&count) && index < count,
            "invalid proof partition"
        );
        Ok(Self { index, count })
    }

    const fn owns(self, ordinal: usize) -> bool {
        ordinal % self.count == self.index
    }

    const fn scope(self) -> Scope {
        Scope::Shard {
            index: self.index,
            count: self.count,
        }
    }
}

fn packages(inventory: &Inventory) -> BTreeSet<Package> {
    inventory
        .targets
        .iter()
        .map(|target| target.package)
        .collect()
}

fn selected(inventory: &Inventory, package: Package, shard: Option<Shard>) -> BTreeSet<String> {
    packages(inventory)
        .into_iter()
        .flat_map(|p| {
            expected(inventory, p)
                .into_iter()
                .map(move |name| (p, name))
        })
        .enumerate()
        .filter(|(ordinal, (p, _))| *p == package && shard.is_none_or(|s| s.owns(*ordinal)))
        .map(|(_, (_, name))| name)
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct DiscoveryReport {
    kani_version: String,
    file_version: String,
    standard_harnesses: BTreeMap<String, Vec<String>>,
    totals: DiscoveryTotals,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct DiscoveryTotals {
    standard_harnesses: usize,
}

struct DiscoverySummary {
    expected: usize,
    reported: usize,
    observed: usize,
    unique: usize,
    missing: usize,
    unexpected: usize,
}

impl DiscoverySummary {
    const fn complete(&self) -> bool {
        self.expected > 0
            && self.reported == self.expected
            && self.observed == self.expected
            && self.unique == self.expected
            && self.missing == 0
            && self.unexpected == 0
    }
}

fn validate_discovery(bytes: &[u8], names: &BTreeSet<String>, version: &str) -> Result<()> {
    let report: DiscoveryReport = serde_json::from_slice(bytes).context("parse Kani discovery")?;
    ensure!(
        report.kani_version == version && report.file_version == "0.1",
        "incompatible Kani discovery report"
    );
    let discovered: Vec<_> = report.standard_harnesses.into_values().flatten().collect();
    let unique: BTreeSet<_> = discovered.iter().cloned().collect();
    let missing: Vec<_> = names.difference(&unique).collect();
    let unexpected: Vec<_> = unique.difference(names).collect();
    let summary = DiscoverySummary {
        expected: names.len(),
        reported: report.totals.standard_harnesses,
        observed: discovered.len(),
        unique: unique.len(),
        missing: missing.len(),
        unexpected: unexpected.len(),
    };
    ensure!(summary.complete(), "compiled proof inventory mismatch: missing {missing:?}; unexpected {unexpected:?}; expected {}, reported {}, discovered {}, unique {}", summary.expected, summary.reported, summary.observed, summary.unique);
    Ok(())
}

fn discover_at(root: &Path, out: &Path, inventory: &Inventory) -> Result<()> {
    for package in packages(inventory) {
        let directory = out.join("discovery").join(package.name());
        fs::create_dir_all(&directory)?;
        let report = directory.join("kani-list.json");
        if report.exists() {
            fs::remove_file(&report)?;
        }
        let status = Command::new("prlimit")
            .current_dir(&directory)
            .arg(format!("--as={}", inventory.address_space_bytes))
            .args(["--", "cargo", "kani", "--manifest-path"])
            .arg(root.join(package.manifest()))
            .args(["--package", package.name()])
            .args(package.selector())
            .arg("--target-dir")
            .arg(out.join("kani"))
            .args(["list", "--format", "json", "-Z", "stubbing"])
            .status()
            .with_context(|| format!("discover compiled Kani harnesses for {}", package.name()))?;
        ensure!(
            status.success(),
            "Kani discovery failed for {}: {status}",
            package.name()
        );
        validate_discovery(
            &fs::read(report)?,
            &expected(inventory, package),
            &inventory.kani_version,
        )
        .with_context(|| format!("validate compiled harnesses for {}", package.name()))?;
    }
    println!("Every registered proof harness is present in the Linux build.");
    Ok(())
}

pub fn discover() -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "Kani discovery requires Linux x64"
    );
    let root = paths::repo_root();
    let inventory = read_inventory(&root)?;
    discover_at(&root, &paths::verification_dir(), &inventory)
}

fn shard_receipt(index: usize) -> String {
    format!("{}/{RECEIPT_NAME}", paths::proof_shard_name(index))
}

pub fn check() -> Result<()> {
    let inventory = read_inventory(&paths::repo_root())?;
    println!(
        "{} registered proof harnesses",
        inventory
            .targets
            .iter()
            .map(|target| target.harnesses.len())
            .sum::<usize>()
    );
    Ok(())
}

pub fn run() -> Result<()> {
    run_subset(None)
}

pub fn run_shard(index: usize, count: usize) -> Result<()> {
    run_subset(Some(Shard::new(index, count)?))
}

struct MemoryTelemetry(Child);

impl MemoryTelemetry {
    fn start() -> Result<Self> {
        Ok(Self(
            Command::new("free")
                .args(["--bytes", "--wide", "--seconds", "10", "--count", "360"])
                .spawn()
                .context("start bounded proof memory telemetry")?,
        ))
    }
}

impl Drop for MemoryTelemetry {
    fn drop(&mut self) {
        if let Err(error) = self.0.kill() {
            eprintln!("Failed to stop proof memory telemetry: {error}");
        }
        if let Err(error) = self.0.wait() {
            eprintln!("Failed to reap proof memory telemetry: {error}");
        }
    }
}

fn run_subset(shard: Option<Shard>) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "Kani requires Linux x64; run `just proofs` on the configured Linux host"
    );
    let root = paths::repo_root();
    let out = shard.map_or_else(paths::verification_dir, |s| paths::proof_shard_dir(s.index));
    fs::create_dir_all(&out)?;
    let receipt = out.join(RECEIPT_NAME);
    if receipt.exists() {
        fs::remove_file(&receipt)?;
    }
    let inventory = read_inventory(&root)?;
    crate::verification_tools::require_bitwuzla(&root)?;
    crate::verification_tools::require_z3(&root)?;
    let version = cmd::capture_stdout(&root, "cargo", &["kani", "--version"])?;
    ensure!(
        version.lines().next()
            == Some(&format!(
                "Kani Rust Verifier {} (cargo plugin)",
                inventory.kani_version
            )),
        "unexpected Kani executable: {version}"
    );
    let before = fingerprint(&root)?;
    discover_at(&root, &out, &inventory)?;
    println!(
        "Proof process virtual address-space limit: {} bytes.",
        inventory.address_space_bytes
    );
    let _telemetry = MemoryTelemetry::start()?;
    let mut results = BTreeMap::new();
    for package in packages(&inventory) {
        let names = selected(&inventory, package, shard);
        if names.is_empty() {
            continue;
        }
        let package_name = package.name();
        let filename = format!("{package_name}.json");
        let result_path = out.join(&filename);
        if result_path.exists() {
            fs::remove_file(&result_path)?;
        }
        let mut command = Command::new("prlimit");
        command
            .current_dir(&root)
            .arg(format!("--as={}", inventory.address_space_bytes))
            .args(["--", "cargo"])
            .args([
                "kani",
                "--manifest-path",
                package.manifest(),
                "--package",
                package_name,
            ])
            .args(package.selector())
            .args([
                "--jobs",
                &inventory.jobs.to_string(),
                "--fail-fast",
                "--output-format",
                "regular",
                "-Z",
                "unstable-options",
                "-Z",
                "stubbing",
                "--harness-timeout",
                &format!("{}s", inventory.timeout_seconds),
                "--target-dir",
            ])
            .arg(out.join("kani"))
            .arg("--export-json")
            .arg(&result_path);
        if shard.is_some() {
            command.arg("--exact");
            for name in &names {
                command.args(["--harness", name]);
            }
        }
        let status = command
            .status()
            .with_context(|| format!("start bounded Kani for {package_name}"))?;
        ensure!(status.success(), "Kani failed for {package_name}: {status}");
        let bytes = fs::read(&result_path).context("Kani produced no report")?;
        validate_report(
            &serde_json::from_slice(&bytes)?,
            &names,
            &inventory.kani_version,
        )?;
        results.insert(filename, sha256_hex(&bytes));
    }
    ensure!(
        fingerprint(&root)? == before,
        "proof source changed during verification"
    );
    ensure!(!results.is_empty(), "empty proof partition");
    fs::write(
        receipt,
        serde_json::to_vec_pretty(&Receipt {
            schema_version: SCHEMA_VERSION,
            source: before,
            results,
            scope: shard.map_or(Scope::Complete, Shard::scope),
        })?,
    )?;
    if let Some(shard) = shard {
        verify_shard(&out, &inventory, shard, &fingerprint(&root)?)?;
        println!("Proof partition {} of {} passed.", shard.index, shard.count);
        Ok(())
    } else {
        verify()
    }
}

pub fn verify() -> Result<()> {
    let root = paths::repo_root();
    let out = paths::verification_dir();
    verify_at(&root, &out)
}

fn verify_at(root: &Path, out: &Path) -> Result<()> {
    let inventory = read_inventory(root)?;
    let receipt = read_receipt(out)?;
    let source = fingerprint(root)?;
    check_source(&receipt, &source)?;
    match receipt.scope {
        Scope::Complete => verify_reports(out, &inventory, &receipt, None)?,
        Scope::Shard { .. } => anyhow::bail!("partial proof receipt cannot establish completeness"),
        Scope::Collection { count } => {
            Shard::new(0, count)?;
            let expected: BTreeSet<_> = (0..count).map(shard_receipt).collect();
            ensure!(
                receipt.results.keys().cloned().collect::<BTreeSet<_>>() == expected,
                "incomplete proof partition receipts"
            );
            for index in 0..count {
                let bytes = verify_shard(
                    &out.join(paths::proof_shard_name(index)),
                    &inventory,
                    Shard::new(index, count)?,
                    &source,
                )?;
                ensure!(
                    receipt.results.get(&shard_receipt(index)) == Some(&sha256_hex(&bytes)),
                    "proof partition receipt changed"
                );
            }
        }
    }
    ensure!(
        fingerprint(root)? == source,
        "proof source changed during verification"
    );
    println!("All registered proofs passed for the current source.");
    Ok(())
}

fn read_receipt(out: &Path) -> Result<Receipt> {
    serde_json::from_slice(
        &fs::read(out.join(RECEIPT_NAME)).context("missing proof evidence; run just proofs")?,
    )
    .context("invalid proof receipt")
}

fn check_source(receipt: &Receipt, source: &BTreeMap<String, String>) -> Result<()> {
    ensure!(
        receipt.schema_version == SCHEMA_VERSION && receipt.source == *source,
        "stale proof evidence"
    );
    Ok(())
}

fn verify_shard(
    out: &Path,
    inventory: &Inventory,
    shard: Shard,
    source: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let bytes = fs::read(out.join(RECEIPT_NAME)).context("missing proof partition receipt")?;
    let receipt: Receipt =
        serde_json::from_slice(&bytes).context("invalid proof partition receipt")?;
    check_source(&receipt, source)?;
    ensure!(
        receipt.scope == shard.scope(),
        "proof partition identity mismatch"
    );
    verify_reports(out, inventory, &receipt, Some(shard))?;
    Ok(bytes)
}

fn verify_reports(
    out: &Path,
    inventory: &Inventory,
    receipt: &Receipt,
    shard: Option<Shard>,
) -> Result<()> {
    let targets: BTreeMap<_, _> = packages(inventory)
        .into_iter()
        .filter_map(|package| {
            let names = selected(inventory, package, shard);
            (!names.is_empty()).then_some((package, names))
        })
        .collect();
    ensure!(!targets.is_empty(), "empty proof partition");
    ensure!(
        receipt.results.keys().cloned().collect::<BTreeSet<_>>()
            == targets
                .keys()
                .map(|package| format!("{}.json", package.name()))
                .collect(),
        "incomplete proof receipts"
    );
    for (package, names) in targets {
        let filename = format!("{}.json", package.name());
        let bytes = fs::read(out.join(&filename))?;
        ensure!(
            receipt.results.get(&filename) == Some(&sha256_hex(&bytes)),
            "proof report changed"
        );
        validate_report(
            &serde_json::from_slice(&bytes)?,
            &names,
            &inventory.kani_version,
        )?;
    }
    Ok(())
}

pub fn collect(count: usize) -> Result<()> {
    collect_at(&paths::repo_root(), &paths::verification_dir(), count)
}

fn collect_at(root: &Path, out: &Path, count: usize) -> Result<()> {
    Shard::new(0, count)?;
    let receipt_path = out.join(RECEIPT_NAME);
    if receipt_path.exists() {
        fs::remove_file(&receipt_path)?;
    }
    let inventory = read_inventory(root)?;
    let source = fingerprint(root)?;
    let mut results = BTreeMap::new();
    for index in 0..count {
        let bytes = verify_shard(
            &out.join(paths::proof_shard_name(index)),
            &inventory,
            Shard::new(index, count)?,
            &source,
        )?;
        let filename = shard_receipt(index);
        results.insert(filename, sha256_hex(&bytes));
    }
    ensure!(
        fingerprint(root)? == source,
        "proof source changed during collection"
    );
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&Receipt {
            schema_version: SCHEMA_VERSION,
            source,
            results,
            scope: Scope::Collection { count },
        })?,
    )?;
    if let Err(error) = verify_at(root, out) {
        fs::remove_file(receipt_path)?;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> KaniReport {
        serde_json::from_value(json!({
            "metadata": {"kani_version":"0.68.0"},
            "verification_results": {
                "summary":{"total_harnesses":1,"executed":1,"successful":1,"failed":0},
                "results":[{"harness_id":"proof","status":"Success","checks":[{"status":"Success","category":"assertion"}]}]
            },
            "property_details":[{"harness_id":"proof","property_details":{
                "total_properties":3,"passed":1,"failed":0,"undetermined":0,"solver_error":0,"satisfied":2,"unsatisfiable":0,"uncovered":0
            }}]
        })).expect("report fixture")
    }

    fn names() -> BTreeSet<String> {
        BTreeSet::from(["proof".into()])
    }

    fn discovery(names: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "kani-version": "0.68.0",
            "file-version": "0.1",
            "standard-harnesses": {"src/proofs.rs": names},
            "totals": {"standard-harnesses": names.len()}
        }))
        .expect("discovery fixture")
    }

    #[test]
    fn compiled_discovery_requires_every_registered_harness_exactly_once() -> Result<()> {
        validate_discovery(&discovery(&["proof"]), &names(), "0.68.0")?;
        for actual in [
            &[][..],
            &["other"][..],
            &["proof", "other"][..],
            &["proof", "proof"][..],
        ] {
            assert!(validate_discovery(&discovery(actual), &names(), "0.68.0").is_err());
        }
        let expected = BTreeSet::from(["proof".into(), "windows_only::proof".into()]);
        let error = validate_discovery(&discovery(&["proof"]), &expected, "0.68.0")
            .expect_err("a declaration excluded from the Linux build must fail discovery");
        assert!(error.to_string().contains("windows_only::proof"));
        assert!(validate_discovery(&discovery(&[]), &BTreeSet::new(), "0.68.0").is_err());
        Ok(())
    }

    #[test]
    fn compiled_discovery_rejects_incompatible_and_inconsistent_reports() -> Result<()> {
        let original: serde_json::Value = serde_json::from_slice(&discovery(&["proof"]))?;
        for (field, replacement) in [
            ("/kani-version", json!("different")),
            ("/file-version", json!("different")),
            ("/totals/standard-harnesses", json!(2)),
            ("/standard-harnesses", json!({})),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(field).expect("fixture field") = replacement;
            assert!(
                validate_discovery(&serde_json::to_vec(&changed)?, &names(), "0.68.0").is_err()
            );
        }
        assert!(validate_discovery(b"{", &names(), "0.68.0").is_err());
        Ok(())
    }

    #[test]
    fn inventory_requires_a_bounded_process_address_space() -> Result<()> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(paths::repo_root().join(MANIFEST_PATH))?)?;
        value["address_space_bytes"] = json!(8_589_934_592_u64);
        let inventory: Inventory = serde_json::from_value(value.clone())
            .expect("the supported address-space budget must be accepted");
        validate_inventory(&inventory)?;
        for limit in [0, 8_589_934_593, u64::MAX] {
            value["address_space_bytes"] = json!(limit);
            let invalid: Inventory = serde_json::from_value(value.clone())?;
            assert!(
                validate_inventory(&invalid).is_err(),
                "invalid process limits must fail before execution"
            );
        }
        Ok(())
    }

    fn fixture_report(names: &BTreeSet<String>, version: &str) -> Result<Vec<u8>> {
        let results: Vec<_> = names.iter().map(|name| json!({
            "harness_id":name,"status":"Success","checks":[{"status":"Success","category":"assertion"}]
        })).collect();
        let details: Vec<_> = names.iter().map(|name| json!({
            "harness_id":name,"property_details":{"total_properties":3,"passed":1,"failed":0,"undetermined":0,"solver_error":0,"satisfied":2,"unsatisfiable":0,"uncovered":0}
        })).collect();
        Ok(serde_json::to_vec(&json!({
            "metadata":{"kani_version":version},
            "verification_results":{"summary":{"total_harnesses":names.len(),"executed":names.len(),"successful":names.len(),"failed":0},"results":results},
            "property_details":details
        }))?)
    }

    #[test]
    fn partitions_cover_every_registered_name_once_and_refuse_invalid_geometry() -> Result<()> {
        let inventory = read_inventory(&paths::repo_root())?;
        for count in 1..=MAX_SHARDS {
            for package in packages(&inventory) {
                let mut union = BTreeSet::new();
                for index in 0..count {
                    for name in selected(&inventory, package, Some(Shard::new(index, count)?)) {
                        assert!(union.insert(name), "partition ownership must be unique");
                    }
                }
                assert_eq!(union, expected(&inventory, package));
            }
            for ordinal in (0..257).chain([usize::MAX]) {
                let owners = (0..count)
                    .filter(|index| {
                        Shard {
                            index: *index,
                            count,
                        }
                        .owns(ordinal)
                    })
                    .count();
                assert_eq!(owners, 1);
            }
        }
        for (index, count) in [(0, 0), (4, 4), (0, 5), (usize::MAX, 4), (0, usize::MAX)] {
            assert!(Shard::new(index, count).is_err());
        }
        Ok(())
    }

    #[test]
    fn complete_partitions_require_unchanged_complete_source_bound_evidence() -> Result<()> {
        let original = paths::repo_root();
        let parent = paths::verification_dir().join("partition-receipt-tests");
        fs::create_dir_all(&parent)?;
        let root = parent.join(std::process::id().to_string());
        fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            for path in fingerprint(&original)?.keys() {
                let destination = root.join(path);
                fs::create_dir_all(destination.parent().context("fixture parent")?)?;
                fs::copy(original.join(path), destination)?;
            }
            let inventory = read_inventory(&root)?;
            let packages: BTreeSet<_> = inventory.targets.iter().map(|t| t.package).collect();
            let all: Vec<_> = packages
                .iter()
                .flat_map(|package| {
                    expected(&inventory, *package)
                        .into_iter()
                        .map(|name| (*package, name))
                })
                .collect();
            let source = fingerprint(&root)?;
            let out = root.join("reports");
            fs::create_dir(&out)?;
            let mut receipts = BTreeMap::new();
            for index in 0..4 {
                let directory = format!("proof-shard-{index}");
                fs::create_dir(out.join(&directory))?;
                let mut reports = BTreeMap::new();
                for package in &packages {
                    let names: BTreeSet<_> = all
                        .iter()
                        .enumerate()
                        .filter(|(ordinal, (p, _))| ordinal % 4 == index && p == package)
                        .map(|(_, (_, name))| name.clone())
                        .collect();
                    if names.is_empty() {
                        continue;
                    }
                    let bytes = fixture_report(&names, &inventory.kani_version)?;
                    let filename = format!("{}.json", package.name());
                    fs::write(out.join(&directory).join(&filename), &bytes)?;
                    reports.insert(filename, sha256_hex(&bytes));
                }
                let receipt = serde_json::to_vec(&json!({
                    "schema_version":SCHEMA_VERSION,"source":source,"results":reports,
                    "scope":{"kind":"shard","index":index,"count":4}
                }))?;
                let filename = format!("{directory}/{RECEIPT_NAME}");
                fs::write(out.join(&filename), &receipt)?;
                receipts.insert(filename, sha256_hex(&receipt));
            }
            let receipt = serde_json::to_vec(&json!({
                "schema_version":SCHEMA_VERSION,"source":source,"results":receipts,
                "scope":{"kind":"collection","count":4}
            }))?;
            let top = out.join(RECEIPT_NAME);
            fs::write(&top, &receipt)?;
            verify_at(&root, &out).expect("every original partition passed for the current source");
            collect_at(&root, &out, 4)?;
            verify_at(&root, &out)?;

            let partial = out.join("proof-shard-0").join(RECEIPT_NAME);
            let partial_bytes = fs::read(&partial)?;
            fs::write(&top, &partial_bytes)?;
            assert!(
                verify_at(&root, &out).is_err(),
                "a partial receipt cannot claim completeness"
            );
            fs::write(&top, &receipt)?;
            fs::remove_file(&partial)?;
            assert!(
                verify_at(&root, &out).is_err(),
                "every partition receipt is required"
            );
            assert!(
                collect_at(&root, &out, 4).is_err(),
                "missing partitions cannot be collected"
            );
            assert!(
                !top.exists(),
                "failed collection must remove an earlier complete receipt"
            );
            fs::write(&partial, &partial_bytes)?;
            fs::write(&top, &receipt)?;

            let mut changed: serde_json::Value = serde_json::from_slice(&partial_bytes)?;
            for (field, value) in [
                ("scope", json!({"kind":"shard","index":1,"count":4})),
                ("source", json!({"different":"source"})),
            ] {
                let saved = changed[field].clone();
                changed[field] = value;
                let bytes = serde_json::to_vec(&changed)?;
                fs::write(&partial, &bytes)?;
                let mut top_value: serde_json::Value = serde_json::from_slice(&receipt)?;
                top_value["results"]["proof-shard-0/proofs.json"] = json!(sha256_hex(&bytes));
                fs::write(&top, serde_json::to_vec(&top_value)?)?;
                assert!(
                    verify_at(&root, &out).is_err(),
                    "partition identity and source must match"
                );
                changed[field] = saved;
            }
            fs::write(&partial, &partial_bytes)?;
            fs::write(&top, &receipt)?;

            let report = out.join("proof-shard-0/fmf-core.json");
            let report_bytes = fs::read(&report)?;
            fs::write(&report, b"{}")?;
            assert!(
                verify_at(&root, &out).is_err(),
                "original report bytes must match"
            );
            fs::write(&report, &report_bytes)?;
            for (pointer, value) in [
                ("/verification_results/summary/executed", json!(0)),
                ("/verification_results/results/0/status", json!("Failure")),
                (
                    "/verification_results/results/0/harness_id",
                    json!("unregistered"),
                ),
                (
                    "/property_details/0/property_details/unsatisfiable",
                    json!(1),
                ),
            ] {
                let mut bad_report: serde_json::Value = serde_json::from_slice(&report_bytes)?;
                *bad_report.pointer_mut(pointer).expect("fixture field") = value;
                let bad_bytes = serde_json::to_vec(&bad_report)?;
                fs::write(&report, &bad_bytes)?;
                let mut partial_value: serde_json::Value = serde_json::from_slice(&partial_bytes)?;
                partial_value["results"]["fmf-core.json"] = json!(sha256_hex(&bad_bytes));
                let changed_partial = serde_json::to_vec(&partial_value)?;
                fs::write(&partial, &changed_partial)?;
                let mut top_value: serde_json::Value = serde_json::from_slice(&receipt)?;
                top_value["results"]["proof-shard-0/proofs.json"] =
                    json!(sha256_hex(&changed_partial));
                fs::write(&top, serde_json::to_vec(&top_value)?)?;
                assert!(
                    verify_at(&root, &out).is_err(),
                    "valid hashes cannot hide unproved harnesses"
                );
                assert!(
                    collect_at(&root, &out, 4).is_err(),
                    "unproved reports cannot be collected"
                );
                assert!(!top.exists());
            }
            fs::write(&report, &report_bytes)?;
            fs::write(&partial, &partial_bytes)?;
            fs::write(&top, &receipt)?;
            let tool_config = root.join("mise.toml");
            let tool_bytes = fs::read(&tool_config)?;
            let mut changed =
                fs::read_to_string(&tool_config)?.parse::<toml_edit::DocumentMut>()?;
            changed["tools"]["rust"] = toml_edit::value("1.99.2");
            fs::write(&tool_config, changed.to_string())?;
            assert!(
                verify_at(&root, &out).is_err(),
                "tool configuration changes invalidate every partition"
            );
            fs::write(tool_config, tool_bytes)?;
            let source_file = root.join("engine/crates/fmf-contract/src/lib.rs");
            fs::write(&source_file, b"changed production source")?;
            assert!(
                verify_at(&root, &out).is_err(),
                "source changes invalidate every partition"
            );
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn rejects_skipped_failed_empty_and_vacuous_runs() {
        assert!(validate_report(&report(), &names(), "0.68.0").is_ok());
        assert!(validate_report(&report(), &BTreeSet::new(), "0.68.0").is_err());
        let mut skipped = report();
        skipped.verification_results.summary.executed = 0;
        assert!(validate_report(&skipped, &names(), "0.68.0").is_err());
        let mut failed = report();
        failed.verification_results.results[0].checks[0].status = "Failure".into();
        assert!(validate_report(&failed, &names(), "0.68.0").is_err());
        let mut vacuous = report();
        vacuous.property_details[0].property_details.unsatisfiable = 1;
        assert!(validate_report(&vacuous, &names(), "0.68.0").is_err());
        let mut absent = report();
        absent.verification_results.results.clear();
        assert!(validate_report(&absent, &names(), "0.68.0").is_err());
    }

    #[test]
    fn rejects_unreachable_assertions_duplicate_results_and_unmeasured_properties() {
        let mut unreachable = report();
        unreachable.verification_results.results[0].checks[0].status = "Unreachable".into();
        assert!(validate_report(&unreachable, &names(), "0.68.0").is_err());
        let mut duplicate = report();
        duplicate.property_details.push(HarnessProperties {
            harness_id: "proof".into(),
            property_details: report().property_details.remove(0).property_details,
        });
        assert!(validate_report(&duplicate, &names(), "0.68.0").is_err());
        assert!(serde_json::from_value::<CheckCounts>(json!({"total_properties":null})).is_err());
    }

    #[test]
    fn parses_rust_attributes_and_requires_assertions_and_explicit_unwinding() {
        let valid = "#[kani::proof] #[kani::unwind(2)] fn check() { assert!(true); kani::cover!(true); kani::cover!(false); }";
        assert_eq!(
            declarations(valid).expect("valid Rust"),
            BTreeSet::from(["check".into()])
        );
        assert!(declarations(&valid.replace("#[kani::unwind(2)]", "")).is_err());
        assert!(declarations(&valid.replace("assert!(true);", "")).is_err());
        assert!(declarations("// #[kani::proof] fn fake() {}\n")
            .expect("comment")
            .is_empty());
    }

    #[test]
    fn inventory_loads_from_declared_repository_files() -> Result<()> {
        let original = paths::repo_root();
        let parent = paths::verification_dir().join("inventory-tests");
        fs::create_dir_all(&parent)?;
        let root = parent.join(std::process::id().to_string());
        fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            for path in [MANIFEST_PATH, "mise.toml"] {
                let destination = root.join(path);
                fs::create_dir_all(destination.parent().context("fixture parent")?)?;
                fs::copy(original.join(path), destination)?;
            }
            let declared: Inventory = serde_json::from_slice(&fs::read(root.join(MANIFEST_PATH))?)?;
            validate_inventory(&declared)?;
            for target in &declared.targets {
                let destination = root.join(&target.source);
                fs::create_dir_all(destination.parent().context("fixture parent")?)?;
                fs::copy(original.join(&target.source), destination)?;
            }
            let loaded = read_inventory(&root)?;
            assert_eq!(loaded.targets.len(), declared.targets.len());
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn receipts_reject_changed_sources_tampered_reports_and_missing_reports() -> Result<()> {
        let original = paths::repo_root();
        let parent = paths::verification_dir().join("receipt-tests");
        fs::create_dir_all(&parent)?;
        let root = parent.join(std::process::id().to_string());
        fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            for path in fingerprint(&original)?.keys() {
                let destination = root.join(path);
                fs::create_dir_all(destination.parent().context("fixture parent")?)?;
                fs::copy(original.join(path), destination)?;
            }
            let inventory = read_inventory(&root)?;
            let out = root.join("reports");
            fs::create_dir(&out)?;
            let mut reports = BTreeMap::new();
            let packages: BTreeSet<_> = inventory
                .targets
                .iter()
                .map(|target| target.package)
                .collect();
            for package in packages {
                let names = expected(&inventory, package);
                let results: Vec<_> = names.iter().map(|name| json!({
                    "harness_id":name,"status":"Success","checks":[{"status":"Success","category":"assertion"}]
                })).collect();
                let details: Vec<_> = names.iter().map(|name| json!({
                    "harness_id":name,"property_details":{"total_properties":3,"passed":1,"failed":0,"undetermined":0,"solver_error":0,"satisfied":2,"unsatisfiable":0,"uncovered":0}
                })).collect();
                let bytes = serde_json::to_vec(&json!({
                    "metadata":{"kani_version":inventory.kani_version},
                    "verification_results":{"summary":{"total_harnesses":names.len(),"executed":names.len(),"successful":names.len(),"failed":0},"results":results},
                    "property_details":details
                }))?;
                let filename = format!("{}.json", package.name());
                fs::write(out.join(&filename), &bytes)?;
                reports.insert(filename, sha256_hex(&bytes));
            }
            fs::write(
                out.join(RECEIPT_NAME),
                serde_json::to_vec(&Receipt {
                    schema_version: SCHEMA_VERSION,
                    source: fingerprint(&root)?,
                    results: reports,
                    scope: Scope::Complete,
                })?,
            )?;
            verify_at(&root, &out)?;
            let tool_config = root.join("mise.toml");
            let tool_bytes = fs::read(&tool_config)?;
            let mut changed =
                fs::read_to_string(&tool_config)?.parse::<toml_edit::DocumentMut>()?;
            changed["tools"]["rust"] = toml_edit::value("1.99.2");
            fs::write(&tool_config, changed.to_string())?;
            assert!(verify_at(&root, &out)
                .expect_err("changed tool configuration must fail")
                .to_string()
                .contains("stale proof evidence"));
            fs::write(tool_config, tool_bytes)?;
            let source = root.join("engine/crates/fmf-contract/src/lib.rs");
            let bytes = fs::read(&source)?;
            fs::write(&source, b"changed production source")?;
            assert!(verify_at(&root, &out)
                .expect_err("changed source must fail")
                .to_string()
                .contains("stale proof evidence"));
            fs::write(source, bytes)?;
            let report = out.join("fmf-contract.json");
            fs::write(&report, b"{}")?;
            assert!(verify_at(&root, &out)
                .expect_err("changed report must fail")
                .to_string()
                .contains("proof report changed"));
            fs::remove_file(report)?;
            assert!(verify_at(&root, &out).is_err());
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
}
