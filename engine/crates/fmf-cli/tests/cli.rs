//! Behavioural tests for the `fmf` binary's non-elevated surface: the parts that need neither an administrator terminal nor a real volume.
//! Anything that touches the $MFT/USN lives behind the `FMF_ADMIN_TESTS` gate elsewhere.

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
fn completions_emit_a_script_for_each_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        fmf()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("fmf"));
    }
}

#[test]
fn completions_reject_an_unknown_shell() {
    fmf()
        .args(["completions", "tcsh"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn no_subcommand_is_a_usage_error() {
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
    let assert = fmf()
        .env("ProgramData", r"C:\TestProgramData")
        .arg("diag")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains(fmf_buildstamp::VERSION));
    assert!(stdout.contains(
        r"engine logs: C:\TestProgramData\find-my-files\logs (rolling engine.<date>.log)"
    ));
    assert!(!stdout.contains(r"\logs\engine.log"));

    assert!(stdout.contains("service process —"), "{stdout}");
    assert!(stdout.contains("this fmf process —"), "{stdout}");
    assert!(
        stdout.contains(r"! cannot read C:\TestProgramData\find-my-files\logs"),
        "{stdout}"
    );
}

#[test]
fn diag_json_is_a_versioned_object() {
    let assert = fmf()
        .env("ProgramData", r"C:\TestProgramData")
        .args(["--format", "json", "diag"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("diag --format json is JSON");
    assert_eq!(v["format_version"].as_u64(), Some(2));
    assert_eq!(v["version"].as_str(), Some(fmf_buildstamp::VERSION));
    assert_eq!(
        v["engine_log_dir"].as_str(),
        Some(r"C:\TestProgramData\find-my-files\logs")
    );
    assert!(v.get("engine_log").is_none());
    assert!(v["recent_errors"].is_array());

    assert_eq!(
        v["service_log"]["dir"].as_str(),
        Some(r"C:\TestProgramData\find-my-files\logs")
    );
    assert!(v["service_log"]["recent"].is_array());
    let notes = v["service_log"]["notes"]
        .as_array()
        .expect("notes is an array");
    assert_eq!(notes.len(), 1, "an empty tail must carry its reason: {v}");
}

#[test]
fn json_format_errors_are_structured() {
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
    assert_eq!(v["format_version"].as_u64(), Some(2));
    assert!(v["error"]["code_num"].is_i64());
    assert!(v["error"]["message"].is_string());
}
