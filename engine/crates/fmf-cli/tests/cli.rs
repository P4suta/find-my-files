//! Behavioural tests for the `fmf` binary's non-elevated surface: the parts
//! that need neither an administrator terminal nor a real volume. Anything
//! that touches the $MFT/USN lives behind the `FMF_ADMIN_TESTS` gate elsewhere.

use assert_cmd::Command;
use predicates::prelude::*;

fn fmf() -> Command {
    Command::cargo_bin("fmf").expect("fmf binary builds")
}

#[test]
fn version_prints_the_package_version() {
    fmf()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_lists_the_subcommands() {
    fmf()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("bench"))
        .stdout(predicate::str::contains("--color"));
}

#[test]
fn no_subcommand_is_a_usage_error() {
    // clap sets exit code 2 for usage errors, before our dispatch runs.
    fmf().assert().failure().code(2);
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    fmf().arg("not-a-real-command").assert().failure().code(2);
}

#[test]
fn color_flag_rejects_an_invalid_value() {
    fmf()
        .args(["--color", "rainbow", "diag"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn diag_runs_unelevated_and_reports_the_version() {
    // `diag` reads versions/log paths/the diag ring — no volume, no admin.
    fmf()
        .arg("diag")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn diag_json_is_a_versioned_object() {
    let assert = fmf().args(["--format", "json", "diag"]).assert().success();
    let v: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("diag --format json is JSON");
    assert_eq!(v["format_version"].as_u64(), Some(1));
    assert_eq!(v["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert!(v["recent_errors"].is_array());
}

#[test]
fn json_format_errors_are_structured() {
    // `index Q:` fails fast (no such volume / not elevated) — either way the
    // failure must surface as a JSON envelope on stderr, as the last line
    // (any diagnostics logging precedes it).
    let assert = fmf()
        .args(["--format", "json", "index", "Q:"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("stderr is not empty");
    let v: serde_json::Value = serde_json::from_str(last).expect("error line is JSON");
    assert_eq!(v["format_version"].as_u64(), Some(1));
    assert!(v["error"]["code_num"].is_i64());
    assert!(v["error"]["message"].is_string());
}
