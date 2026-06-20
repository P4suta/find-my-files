//! `just doctor` — a fast check that the dev environment matches `mise.toml`
//! and the gate prerequisites, so a contributor knows right after `just setup`
//! whether anything is off.
//!
//! The pure helpers (pin parsing, version matching, rendering) are unit-tested;
//! `run` is the only part that shells out to the tools.

use std::collections::BTreeMap;
use std::fmt::Write as _;

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
    fn warn(name: &str, detail: &str) -> Self {
        Self::new(Status::Warn, name, detail)
    }
    fn fail(name: &str, detail: &str) -> Self {
        Self::new(Status::Fail, name, detail)
    }
}

// Pull the bare-name tool pins out of a mise.toml `[tools]` table (rust, dotnet,
// just, lefthook). Keys carrying a backend prefix (`cargo:`, `github:`) and
// non-string values are skipped — doctor only probes tools it can run directly.
fn parse_mise_pins(mise_toml: &str) -> BTreeMap<String, String> {
    let mut pins = BTreeMap::new();
    let Ok(doc) = mise_toml.parse::<DocumentMut>() else {
        return pins;
    };
    let Some(tools) = doc.get("tools").and_then(|t| t.as_table()) else {
        return pins;
    };
    for (key, value) in tools {
        if key.contains(':') {
            continue;
        }
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

// The first whitespace-separated token that starts with a digit — pulls
// "1.95.0" out of "rustc 1.95.0 (hash 2026-..)" and leaves "10.0.118" intact.
fn first_version_token(raw: &str) -> Option<&str> {
    raw.split_whitespace()
        .find(|tok| tok.as_bytes().first().is_some_and(u8::is_ascii_digit))
}

// Probe one tool: compare the version it reports against its mise pin.
fn tool_check(name: &str, program: &str, args: &[&str], pins: &BTreeMap<String, String>) -> Check {
    let Some(pin) = pins.get(name) else {
        return Check::warn(name, "not pinned in mise.toml");
    };
    let Some(raw) = cmd::capture(&paths::repo_root(), program, args) else {
        return Check::warn(
            name,
            &format!("pinned {pin}, but `{program}` is not on PATH — run `mise install`"),
        );
    };
    let Some(actual) = first_version_token(&raw) else {
        return Check::warn(
            name,
            &format!("pinned {pin}, but `{program}` reported an unparsable version"),
        );
    };
    if version_satisfies(pin, actual) {
        Check::ok(name, &format!("{actual} (pin {pin})"))
    } else {
        Check::warn(
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

// ADR-0021: every build artifact lives under build/. A resurrected engine/target
// or xtask/target is a regression worth flagging.
fn build_layout_check() -> Check {
    let root = paths::repo_root();
    let strays: Vec<&str> = ["engine/target", "xtask/target"]
        .into_iter()
        .filter(|&rel| root.join(rel).exists())
        .collect();
    if strays.is_empty() {
        Check::ok("build/ layout", "no stray target/ dirs (ADR-0021)")
    } else {
        let dirs = strays.join(", ");
        Check::fail(
            "build/ layout",
            &format!("stray cargo target dir(s): {dirs} — delete; output belongs under build/ (ADR-0021)"),
        )
    }
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

    for (name, program, args) in [
        ("rust", "rustc", &["--version"][..]),
        ("dotnet", "dotnet", &["--version"][..]),
        ("just", "just", &["--version"][..]),
        ("lefthook", "lefthook", &["version"][..]),
    ] {
        checks.push(tool_check(name, program, args, &pins));
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
    fn parses_bare_pins_and_skips_backends() {
        let toml = "\
[tools]
rust = \"1.95\"
dotnet = \"10\"
just = \"1\"
\"cargo:samply\" = \"0.13.1\"
\"github:rhysd/actionlint\" = \"1.7.7\"

[settings]
cargo.binstall = true
";
        let pins = parse_mise_pins(toml);
        assert_eq!(pins.get("rust").map(String::as_str), Some("1.95"));
        assert_eq!(pins.get("dotnet").map(String::as_str), Some("10"));
        assert_eq!(pins.get("just").map(String::as_str), Some("1"));
        assert!(!pins.contains_key("cargo:samply"));
        assert!(!pins.contains_key("github:rhysd/actionlint"));
        assert_eq!(pins.len(), 3);
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
        assert_eq!(first_version_token("no version here"), None);
    }

    #[test]
    fn overall_is_the_worst_status() {
        let ok = vec![Check::ok("a", ""), Check::info("b", "")];
        assert_eq!(overall(&ok), Status::Ok);
        let warn = vec![Check::ok("a", ""), Check::warn("b", "")];
        assert_eq!(overall(&warn), Status::Warn);
        let fail = vec![Check::warn("a", ""), Check::fail("b", "")];
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
}
