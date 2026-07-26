//! `just doctor` — a fast check that the dev environment matches `mise.toml`
//! and the gate prerequisites, so a contributor knows right after `just setup`
//! whether anything is off.
//!
//! The pure helpers (pin parsing, version matching, rendering) are unit-tested;
//! `run` is the only part that shells out to the tools.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Result};
use toml_edit::DocumentMut;

use crate::{cmd, paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Status {
    const fn tag(self) -> &'static str {
        match self {
            Self::Ok => "[ OK ]",
            Self::Info => "[INFO]",
            Self::Warn => "[WARN]",
            Self::Fail => "[FAIL]",
        }
    }
}

struct Check {
    name: String,
    status: Status,
    detail: String,
}

impl Check {
    fn new(status: Status, name: &str, detail: &str) -> Self {
        Self {
            name: name.to_owned(),
            status,
            detail: detail.to_owned(),
        }
    }
    fn ok(name: &str, detail: &str) -> Self {
        Self::new(Status::Ok, name, detail)
    }
    fn info(name: &str, detail: &str) -> Self {
        Self::new(Status::Info, name, detail)
    }
    fn fail(name: &str, detail: &str) -> Self {
        Self::new(Status::Fail, name, detail)
    }
}

// Pull every string tool pin out of mise.toml. Backend-qualified tools are kept:
// a required cargo subcommand must not disappear behind a falsely green doctor.
fn parse_mise_pins(mise_toml: &str) -> BTreeMap<String, String> {
    let mut pins = BTreeMap::new();
    let Ok(doc) = mise_toml.parse::<DocumentMut>() else {
        return pins;
    };
    let Some(tools) = doc.get("tools").and_then(|t| t.as_table()) else {
        return pins;
    };
    for (key, value) in tools {
        if let Some(v) = value.as_str() {
            pins.insert(key.to_owned(), v.to_owned());
        }
    }
    pins
}

// Whether `actual` satisfies the loose mise `pin`: each dot-separated part of the
// pin must equal the matching leading part of `actual`. Pin `10` accepts
// `10.0.118`; pin `1.95` accepts `1.95.3` but not `1.96.0`.
fn version_satisfies(pin: &str, actual: &str) -> bool {
    let mut actual_parts = actual.split('.');
    for p in pin.split('.') {
        if actual_parts.next() != Some(p) {
            return false;
        }
    }
    true
}

// The first whitespace-separated version token. Accept Node-style `v24.1.0`
// as well as `rustc 1.95.0` and return the numeric part.
fn first_version_token(raw: &str) -> Option<&str> {
    raw.split_whitespace().find_map(|tok| {
        let numeric = tok.strip_prefix('v').unwrap_or(tok);
        numeric
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
            .then_some(numeric)
    })
}

// Probe one tool: compare the version it reports against its mise pin.
fn tool_check(
    name: &str,
    pin_key: &str,
    program: &str,
    args: &[&str],
    pins: &BTreeMap<String, String>,
) -> Check {
    let Some(pin) = pins.get(pin_key) else {
        return Check::fail(name, "not pinned in mise.toml");
    };
    let Some(raw) = cmd::capture(&paths::repo_root(), program, args) else {
        return Check::fail(
            name,
            &format!("pinned {pin}, but `{program}` is not on PATH — run `mise install`"),
        );
    };
    let Some(actual) = first_version_token(&raw) else {
        return Check::fail(
            name,
            &format!("pinned {pin}, but `{program}` reported an unparsable version"),
        );
    };
    if version_satisfies(pin, actual) {
        Check::ok(name, &format!("{actual} (pin {pin})"))
    } else {
        Check::fail(
            name,
            &format!("pinned {pin}, found {actual} — run `mise install`"),
        )
    }
}

#[cfg(windows)]
fn elevation_detail() -> String {
    // High Mandatory Level (S-1-16-12288) or System (S-1-16-16384) in whoami's
    // group list means an elevated token — no extra crate dependency needed.
    match cmd::capture(&paths::repo_root(), "whoami", &["/groups"]) {
        Some(groups) if groups.contains("S-1-16-12288") || groups.contains("S-1-16-16384") => {
            "ADMIN — full $MFT / USN access".to_owned()
        }
        Some(_) => "standard — index / bench / service recipes need an elevated shell".to_owned(),
        None => "unknown (whoami unavailable)".to_owned(),
    }
}

#[cfg(not(windows))]
fn elevation_detail() -> String {
    "n/a (non-Windows host)".to_owned()
}

// ADR-0021: every generated artifact except C# obj lives under build/. The
// required `build/` ignore rule also matches nested directories, and the WinUI
// project's local ignore file masks its conventional output directories, so
// doctor probes these paths directly instead of relying on `git status`.
const MISPLACED_OUTPUT_PATHS: &[&str] = &[
    "target",
    "engine/target",
    "xtask/target",
    "engine/fuzz/target",
    "engine/engine-lcov.info",
    "engine/mutants.out",
    "engine/mutants.out.old",
    "docfx/build",
    "app/FindMyFiles.Tests/build",
    "app/FindMyFiles.Tests/coverage.json",
    "app/FindMyFiles.Tests/TestResults",
    "app/FindMyFiles.Tests/StrykerOutput",
    "app/FindMyFiles.Tests/UiAutomation/artifacts",
];

const APP_LOCAL_OUTPUT_DIR_NAMES: &[&str] = &[
    "bin",
    "debug",
    "release",
    "artifacts",
    "log",
    "logs",
    "AppPackages",
    "BundleArtifacts",
    "publish",
    "PublishScripts",
    "Generated Files",
];

fn is_app_local_output_dir(name: &str) -> bool {
    APP_LOCAL_OUTPUT_DIR_NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || name
            .get(.."testresult".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("testresult"))
}

fn build_layout_strays(root: &Path) -> Vec<String> {
    let mut strays: Vec<String> = MISPLACED_OUTPUT_PATHS
        .iter()
        .filter(|rel| root.join(rel).exists())
        .map(|rel| (*rel).to_owned())
        .collect();

    let app_dir = root.join("app").join("FindMyFiles");
    if let Ok(entries) = std::fs::read_dir(app_dir) {
        for entry in entries.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_app_local_output_dir(&name) {
                strays.push(format!("app/FindMyFiles/{name}"));
            }
        }
    }

    strays.sort();
    strays.dedup();
    strays
}

fn build_layout_check_at(root: &Path) -> Check {
    let strays = build_layout_strays(root);
    if strays.is_empty() {
        Check::ok(
            "build/ layout",
            "no generated output outside build/ (ADR-0021)",
        )
    } else {
        let paths = strays.join(", ");
        Check::fail(
            "build/ layout",
            &format!(
                "stray generated path(s): {paths} — delete; output belongs under build/ (ADR-0021)"
            ),
        )
    }
}

fn build_layout_check() -> Check {
    let root = paths::repo_root();
    build_layout_check_at(&root)
}

fn render(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::from("\nfind-my-files doctor\n\n");
    for c in checks {
        let _ = writeln!(
            out,
            "  {tag}  {name:<width$}  {detail}",
            tag = c.status.tag(),
            name = c.name,
            detail = c.detail,
        );
    }
    out
}

fn overall(checks: &[Check]) -> Status {
    if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Ok
    }
}

/// Print the environment report and fail (non-zero exit) only when a `[FAIL]`
/// item is present — a `[WARN]` leaves the environment usable.
pub fn run() -> Result<()> {
    let pins = match std::fs::read_to_string(paths::mise_toml()) {
        Ok(text) => parse_mise_pins(&text),
        Err(_) => BTreeMap::new(),
    };

    let mise = if matches!(
        cmd::succeeds(&paths::repo_root(), "mise", &["--version"]),
        Ok(true)
    ) {
        Check::ok("mise", "present")
    } else {
        Check::fail(
            "mise",
            "not found — install mise, then `mise install` (see CONTRIBUTING)",
        )
    };
    let mut checks = vec![mise];

    for (name, pin_key, program, args) in [
        ("rust", "rust", "rustc", &["--version"][..]),
        ("dotnet", "dotnet", "dotnet", &["--version"][..]),
        (
            "node",
            "node",
            "mise",
            &["exec", "node", "--command", "node --version"][..],
        ),
        (
            "winapp",
            "npm:@microsoft/winappcli",
            "winapp",
            &["--version"][..],
        ),
        ("just", "just", "just", &["--version"][..]),
        ("lefthook", "lefthook", "lefthook", &["version"][..]),
        (
            "cargo-llvm-cov",
            "cargo:cargo-llvm-cov",
            "cargo",
            &["llvm-cov", "--version"][..],
        ),
        (
            "cargo-nextest",
            "cargo:cargo-nextest",
            "cargo",
            &["nextest", "--version"][..],
        ),
        (
            "cargo-deny",
            "cargo:cargo-deny",
            "cargo",
            &["deny", "--version"][..],
        ),
        (
            "cargo-machete",
            "cargo:cargo-machete",
            "cargo-machete",
            &["--version"][..],
        ),
        (
            "cargo-about",
            "cargo:cargo-about",
            "cargo",
            &["about", "--version"][..],
        ),
        (
            "cargo-mutants",
            "cargo:cargo-mutants",
            "cargo",
            &["mutants", "--version"][..],
        ),
        ("samply", "cargo:samply", "samply", &["--version"][..]),
        ("bacon", "cargo:bacon", "bacon", &["--version"][..]),
        (
            "mdbook",
            "cargo:mdbook",
            "mise",
            &["exec", "cargo:mdbook", "--command", "mdbook --version"][..],
        ),
        ("taplo", "cargo:taplo-cli", "taplo", &["--version"][..]),
        ("typos", "cargo:typos-cli", "typos", &["--version"][..]),
        (
            "committed",
            "cargo:committed",
            "committed",
            &["--version"][..],
        ),
        (
            "actionlint",
            "github:rhysd/actionlint",
            "actionlint",
            &["--version"][..],
        ),
        (
            "zizmor",
            "github:zizmorcore/zizmor",
            "mise",
            &[
                "exec",
                "github:zizmorcore/zizmor",
                "--command",
                "zizmor --version",
            ][..],
        ),
    ] {
        checks.push(tool_check(name, pin_key, program, args, &pins));
    }

    checks.push(Check::info("elevation", &elevation_detail()));
    checks.push(build_layout_check());

    print!("{}", render(&checks));

    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    let summary = match overall(&checks) {
        Status::Fail => format!("\n{fails} FAIL, {warns} WARN — fix the failures above\n"),
        Status::Warn => {
            format!("\n{warns} WARN — environment usable; `mise install` resolves version drift\n")
        }
        Status::Ok | Status::Info => "\nall good — environment matches mise.toml\n".to_owned(),
    };
    print!("{summary}");

    if fails > 0 {
        bail!("doctor found {fails} environment failure(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_backend_pins() {
        let toml = "\
[tools]
rust = \"1.95\"
dotnet = \"10\"
node = \"24\"
\"npm:@microsoft/winappcli\" = \"0.3.1\"
just = \"1\"
\"cargo:samply\" = \"0.13.1\"
\"cargo:cargo-nextest\" = \"0.9.140\"
\"github:rhysd/actionlint\" = \"1.7.7\"
\"github:zizmorcore/zizmor\" = \"1.28.0\"

[settings]
cargo.binstall = true
";
        let pins = parse_mise_pins(toml);
        assert_eq!(pins.get("rust").map(String::as_str), Some("1.95"));
        assert_eq!(pins.get("dotnet").map(String::as_str), Some("10"));
        assert_eq!(pins.get("node").map(String::as_str), Some("24"));
        assert_eq!(
            pins.get("npm:@microsoft/winappcli").map(String::as_str),
            Some("0.3.1")
        );
        assert_eq!(pins.get("just").map(String::as_str), Some("1"));
        assert_eq!(pins.get("cargo:samply").map(String::as_str), Some("0.13.1"));
        assert_eq!(
            pins.get("cargo:cargo-nextest").map(String::as_str),
            Some("0.9.140")
        );
        assert_eq!(
            pins.get("github:rhysd/actionlint").map(String::as_str),
            Some("1.7.7")
        );
        assert_eq!(
            pins.get("github:zizmorcore/zizmor").map(String::as_str),
            Some("1.28.0")
        );
        assert_eq!(pins.len(), 9);
    }

    #[test]
    fn version_satisfies_loose_pins() {
        assert!(version_satisfies("10", "10.0.118"));
        assert!(version_satisfies("1.95", "1.95.0"));
        assert!(version_satisfies("1.95", "1.95.3"));
        assert!(version_satisfies("1", "1.53.0"));
        assert!(version_satisfies("1.53.0", "1.53.0"));
        assert!(!version_satisfies("1.96", "1.95.0"));
        assert!(!version_satisfies("1.95", "1.9"));
        assert!(!version_satisfies("1.95.0", "1.95"));
    }

    #[test]
    fn pulls_version_token_from_tool_output() {
        assert_eq!(
            first_version_token("rustc 1.95.0 (abc 2026-01-01)"),
            Some("1.95.0")
        );
        assert_eq!(first_version_token("just 1.53.0"), Some("1.53.0"));
        assert_eq!(first_version_token("10.0.118"), Some("10.0.118"));
        assert_eq!(first_version_token("v24.4.0"), Some("24.4.0"));
        assert_eq!(first_version_token("no version here"), None);
    }

    #[test]
    fn overall_is_the_worst_status() {
        let ok = vec![Check::ok("a", ""), Check::info("b", "")];
        assert_eq!(overall(&ok), Status::Ok);
        let warn = vec![Check::ok("a", ""), Check::new(Status::Warn, "b", "")];
        assert_eq!(overall(&warn), Status::Warn);
        let fail = vec![Check::new(Status::Warn, "a", ""), Check::fail("b", "")];
        assert_eq!(overall(&fail), Status::Fail);
    }

    #[test]
    fn render_shows_each_status_tag() {
        let checks = vec![Check::ok("alpha", "fine"), Check::fail("beta", "broken")];
        let out = render(&checks);
        assert!(out.contains("[ OK ]"));
        assert!(out.contains("[FAIL]"));
        assert!(out.contains("alpha"));
        assert!(out.contains("broken"));
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xtask-doctor-{tag}-{}", std::process::id()))
    }

    #[test]
    fn build_layout_allows_root_build_and_csharp_obj() {
        let root = scratch("layout-allowed");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build").join("engine")).unwrap();
        std::fs::create_dir_all(root.join("app").join("FindMyFiles").join("obj")).unwrap();
        std::fs::create_dir_all(root.join("app").join("FindMyFiles.Tests").join("obj")).unwrap();

        let check = build_layout_check_at(&root);

        assert_eq!(check.status, Status::Ok);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_layout_detects_nested_build_directories_hidden_by_ignore_rule() {
        let root = scratch("layout-nested-build");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docfx").join("build")).unwrap();
        let coverage = root
            .join("app")
            .join("FindMyFiles.Tests")
            .join("build")
            .join("cov.xml");
        std::fs::create_dir_all(coverage.parent().unwrap()).unwrap();
        std::fs::write(coverage, b"coverage").unwrap();

        let check = build_layout_check_at(&root);

        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("docfx/build"));
        assert!(check.detail.contains("app/FindMyFiles.Tests/build"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_layout_detects_app_local_outputs_but_not_obj() {
        let root = scratch("layout-app-output");
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("app").join("FindMyFiles");
        for name in [
            "Bin",
            "Debug",
            "Release",
            "artifacts",
            "Logs",
            "AppPackages",
            "BundleArtifacts",
            "publish",
            "PublishScripts",
            "Generated Files",
            "TestResults-2026",
        ] {
            std::fs::create_dir_all(app.join(name)).unwrap();
        }
        std::fs::create_dir_all(app.join("obj")).unwrap();

        let check = build_layout_check_at(&root);

        assert_eq!(check.status, Status::Fail);
        for name in [
            "Bin",
            "Debug",
            "Release",
            "artifacts",
            "Logs",
            "AppPackages",
            "BundleArtifacts",
            "publish",
            "PublishScripts",
            "Generated Files",
            "TestResults-2026",
        ] {
            assert!(
                check.detail.contains(&format!("app/FindMyFiles/{name}")),
                "missing {name} from {}",
                check.detail
            );
        }
        assert!(!check.detail.contains("app/FindMyFiles/obj"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
