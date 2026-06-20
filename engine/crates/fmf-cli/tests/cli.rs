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
