//! Check the regular expression prefilter setting in fresh processes.

use std::process::Command;

use fmf_runtime::query::{CaseMode, UtcResolver, compile, parse};

const EXPECTED_DRIVER: &str = "FMF_TEST_PREFILTER_EXPECTED_DRIVER";
const CHILD_COMPLETED: &str = "prefilter contract checked";

#[test]
fn prefilter_environment_child() {
    let Ok(expected) = std::env::var(EXPECTED_DRIVER) else {
        return;
    };
    let query = compile(
        &parse(r"regex:report[0-9]+").unwrap(),
        CaseMode::Smart,
        &UtcResolver,
    )
    .unwrap();
    assert_eq!(query.driver_label(), expected);
    let no_literal = compile(&parse(r"regex:\d+").unwrap(), CaseMode::Smart, &UtcResolver).unwrap();
    assert_eq!(no_literal.driver_label(), "full-scan");
    println!("{CHILD_COMPLETED}");
}

#[test]
fn prefilter_environment_is_checked_in_a_fresh_process() {
    for (setting, expected) in [
        (None, "pool-scan"),
        (Some("0"), "full-scan"),
        (Some("1"), "pool-scan"),
        (Some("false"), "pool-scan"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "prefilter_environment_child", "--nocapture"]);
        child.env(EXPECTED_DRIVER, expected);
        child.env_remove("FMF_REGEX_PREFILTER");
        if let Some(value) = setting {
            child.env("FMF_REGEX_PREFILTER", value);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{setting:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(CHILD_COMPLETED),
            "child did not check {setting:?}"
        );
    }
}
