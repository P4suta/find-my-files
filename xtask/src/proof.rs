use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use syn::visit::Visit;

use crate::{checksum::sha256_hex, cmd, paths};

const SCHEMA_VERSION: u32 = 1;
const MAX_JOBS: usize = 2;
const MAX_TIMEOUT_SECONDS: u32 = 90;
const MANIFEST_PATH: &str = "verification/proofs.json";
const RECEIPT_NAME: &str = "proofs.json";
const MIN_COVERS: usize = 2;
const QUALIFIED_ATTRIBUTE_SEGMENTS: usize = 2;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    schema_version: u32,
    kani_version: String,
    jobs: usize,
    timeout_seconds: u32,
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
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "Kani requires Linux x64; run `just proofs` on the configured Linux host"
    );
    let root = paths::repo_root();
    let out = paths::verification_dir();
    fs::create_dir_all(&out)?;
    let receipt = out.join(RECEIPT_NAME);
    if receipt.exists() {
        fs::remove_file(&receipt)?;
    }
    let inventory = read_inventory(&root)?;
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
    let mut results = BTreeMap::new();
    let packages: BTreeSet<_> = inventory
        .targets
        .iter()
        .map(|target| target.package)
        .collect();
    for package in packages {
        let package_name = package.name();
        let filename = format!("{package_name}.json");
        let result_path = out.join(&filename);
        if result_path.exists() {
            fs::remove_file(&result_path)?;
        }
        let status = Command::new("cargo")
            .current_dir(&root)
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
                "--output-format",
                "terse",
                "-Z",
                "unstable-options",
                "--harness-timeout",
                &format!("{}s", inventory.timeout_seconds),
                "--target-dir",
            ])
            .arg(out.join("kani"))
            .arg("--export-json")
            .arg(&result_path)
            .status()?;
        ensure!(status.success(), "Kani failed for {package_name}: {status}");
        let bytes = fs::read(&result_path).context("Kani produced no report")?;
        validate_report(
            &serde_json::from_slice(&bytes)?,
            &expected(&inventory, package),
            &inventory.kani_version,
        )?;
        results.insert(filename, sha256_hex(&bytes));
    }
    ensure!(
        fingerprint(&root)? == before,
        "proof source changed during verification"
    );
    fs::write(
        receipt,
        serde_json::to_vec_pretty(&Receipt {
            schema_version: SCHEMA_VERSION,
            source: before,
            results,
        })?,
    )?;
    verify()
}

pub fn verify() -> Result<()> {
    let root = paths::repo_root();
    let out = paths::verification_dir();
    verify_at(&root, &out)
}

fn verify_at(root: &Path, out: &Path) -> Result<()> {
    let inventory = read_inventory(root)?;
    let receipt: Receipt = serde_json::from_slice(
        &fs::read(out.join(RECEIPT_NAME)).context("missing proof evidence; run just proofs")?,
    )?;
    ensure!(
        receipt.schema_version == SCHEMA_VERSION && receipt.source == fingerprint(root)?,
        "stale proof evidence"
    );
    let packages: BTreeSet<_> = inventory
        .targets
        .iter()
        .map(|target| target.package)
        .collect();
    ensure!(
        receipt.results.keys().cloned().collect::<BTreeSet<_>>()
            == packages
                .iter()
                .map(|package| format!("{}.json", package.name()))
                .collect(),
        "incomplete proof receipts"
    );
    for package in packages {
        let filename = format!("{}.json", package.name());
        let bytes = fs::read(out.join(&filename))?;
        ensure!(
            receipt.results.get(&filename) == Some(&sha256_hex(&bytes)),
            "proof report changed"
        );
        validate_report(
            &serde_json::from_slice(&bytes)?,
            &expected(&inventory, package),
            &inventory.kani_version,
        )?;
    }
    println!("All registered proofs passed for the current source.");
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
                })?,
            )?;
            verify_at(&root, &out)?;
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
