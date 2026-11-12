use std::collections::BTreeSet;

use yaml_rust2::{Yaml, YamlLoader};

const CI: &str = include_str!("../../.github/workflows/ci.yml");
const FUZZ: &str = include_str!("../../.github/workflows/fuzz.yml");
const ADMIN: &str = include_str!("../../.github/workflows/nightly-admin.yml");
const CODEQL: &str = include_str!("../../.github/workflows/codeql.yml");
const AUDIT: &str = include_str!("../../.github/workflows/cargo-audit.yml");

#[test]
fn pre_push_uses_the_native_gate_without_shell_conditionals() {
    let hooks = yaml(include_str!("../../lefthook.yml"));
    let verify = hooks["pre-push"]["jobs"]
        .as_vec()
        .expect("push jobs")
        .iter()
        .find(|job| job["name"].as_str() == Some("verify"))
        .expect("verification job");
    assert_eq!(verify["run"].as_str(), Some("just verify-push"));
    assert!(include_str!("../../justfile")
        .contains("just {{ if os() == \"windows\" { \"verify\" } else { \"verify-portable\" } }}"));
    let fixture = include_str!("../../justfile")
        .split_once("build-ffi-for-source-gate:\n")
        .expect("native fixture recipe")
        .1
        .split_once("\n\n")
        .expect("fixture recipe end")
        .0;
    assert!(fixture.contains("--target-dir ../build/engine"));
}

fn yaml(source: &str) -> Yaml {
    let mut documents = YamlLoader::load_from_str(source).expect("valid YAML");
    assert_eq!(documents.len(), 1, "one YAML document");
    documents.remove(0)
}

#[test]
fn only_fast_gates_use_changed_path_filters() {
    let ci = yaml(CI);
    let filter_step = ci["jobs"]["changes"]["steps"]
        .as_vec()
        .expect("changes steps")
        .iter()
        .find(|step| step["id"].as_str() == Some("f"))
        .expect("path filter");
    let filters = yaml(filter_step["with"]["filters"].as_str().expect("filters"));
    let names: BTreeSet<_> = filters
        .as_hash()
        .expect("filter map")
        .keys()
        .map(|name| name.as_str().expect("filter name"))
        .collect();
    assert_eq!(names, BTreeSet::from(["bench", "code", "comments"]));
    for name in names {
        let expected = format!("${{{{ inputs.full_assurance || steps.f.outputs.{name} }}}}");
        assert_eq!(
            ci["jobs"]["changes"]["outputs"][name].as_str(),
            Some(expected.as_str())
        );
    }
}

#[test]
fn ci_artifacts_expire_within_seven_days() {
    let ci = yaml(CI);
    let mut uploads = 0;
    for job in ci["jobs"].as_hash().expect("CI jobs").values() {
        for step in job["steps"].as_vec().into_iter().flatten() {
            if step["uses"]
                .as_str()
                .is_some_and(|action| action.starts_with("actions/upload-artifact@"))
            {
                uploads += 1;
                let days = step["with"]["retention-days"]
                    .as_i64()
                    .expect("fixed artifact retention");
                assert!((1..=7).contains(&days), "artifact retention: {days}");
            }
        }
    }
    assert!(uploads > 0);
}

#[test]
fn fuzz_uses_one_runner_and_never_duplicates_a_main_push() {
    let fuzz = yaml(FUZZ);
    assert!(fuzz["on"]["push"].is_badvalue(), "no main fuzz campaign");
    assert!(fuzz["on"]["schedule"].is_badvalue());
    assert!(!fuzz["on"]["workflow_dispatch"].is_badvalue());
    assert_eq!(fuzz["jobs"].as_hash().expect("fuzz jobs").len(), 1);
    assert!(
        fuzz["jobs"]["fuzz"]["strategy"].is_badvalue(),
        "no target matrix"
    );
    assert_eq!(FUZZ.matches("cargo install cargo-fuzz").count(), 1);
    let ci = yaml(CI);
    assert_eq!(
        ci["jobs"]["fuzz"]["if"].as_str(),
        Some("needs.changes.outputs.fuzz == 'true'")
    );
    assert!(ci["jobs"]["ci-required"]["needs"]
        .as_vec()
        .expect("required needs")
        .contains(&Yaml::String("fuzz".to_owned())));
    assert_eq!(
        fuzz["jobs"]["fuzz"]["env"]["FUZZ_SECONDS"].as_str(),
        Some("60")
    );
    let lock_check = fuzz["jobs"]["fuzz"]["steps"]
        .as_vec()
        .expect("fuzz steps")
        .iter()
        .find(|step| step["name"].as_str() == Some("Verify committed fuzz dependency graph"))
        .expect("locked graph check");
    let command = lock_check["run"].as_str().expect("lock check command");
    assert!(command.contains("cargo metadata --locked"));
    assert!(
        !command.contains("--no-deps"),
        "resolve transitive dependencies before fuzzing"
    );
}

#[test]
fn every_fuzz_binary_runs_once_through_the_shared_action() {
    let fuzz = yaml(FUZZ);
    let steps = fuzz["jobs"]["fuzz"]["steps"].as_vec().expect("fuzz steps");
    let targets: Vec<&str> = steps
        .iter()
        .filter_map(|step| step["with"]["target"].as_str())
        .collect();
    let manifest: toml_edit::DocumentMut = include_str!("../../engine/fuzz/Cargo.toml")
        .parse()
        .expect("fuzz manifest");
    let expected: BTreeSet<&str> = manifest["bin"]
        .as_array_of_tables()
        .expect("fuzz binaries")
        .iter()
        .map(|bin| bin["name"].as_str().expect("target name"))
        .collect();
    assert_eq!(targets.iter().copied().collect::<BTreeSet<_>>(), expected);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository");
    for name in &expected {
        let corpus = root.join("engine/fuzz/corpus").join(name);
        assert!(
            std::fs::read_dir(&corpus)
                .expect("committed corpus directory")
                .any(|entry| {
                    let entry = entry.expect("corpus entry");
                    entry.file_type().expect("corpus type").is_file()
                        && entry.metadata().expect("corpus metadata").len() > 0
                }),
            "missing saved input for {name}"
        );
    }
    let action = yaml(include_str!("../../.github/actions/fuzz-target/action.yml"));
    let runs: Vec<&str> = action["runs"]["steps"]
        .as_vec()
        .expect("fuzz steps")
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect();
    assert!(
        runs.iter()
            .any(|run| run.contains("--manifest-path xtask/Cargo.toml")
                && run.contains("fuzz \"$FUZZ_TARGET\" \"$FUZZ_SECONDS\"")),
        "use the shared runner for replay and bounded exploration"
    );
    assert!(runs.iter().all(|run| !run.contains("cargo fuzz")));
    assert_eq!(
        targets.len(),
        expected.len(),
        "no duplicate target executions"
    );
    for step in steps
        .iter()
        .filter(|step| step["with"]["target"].as_str().is_some())
    {
        assert_eq!(step["uses"].as_str(), Some("$/.github/actions/fuzz-target"));
        assert!(
            !step["if"].is_badvalue(),
            "each target must select its surface"
        );
    }
}

#[test]
fn automatic_admin_runs_are_weekly_and_benchmarks_only_smoke() {
    let admin = yaml(ADMIN);
    assert_eq!(
        admin["on"]["schedule"][0]["cron"].as_str(),
        Some("17 17 * * 1")
    );
    assert!(!admin["on"]["workflow_dispatch"].is_badvalue());
    assert!(CI.contains("cargo bench --locked -p fmf-runtime --features testutil -- --test"));
}

#[test]
fn static_and_dependency_scans_keep_their_checks_but_filter_pr_inputs() {
    let codeql = yaml(CODEQL);
    assert!(!codeql["on"]["schedule"].is_badvalue());
    for job in ["analyze", "analyze-actions"] {
        assert_eq!(codeql["jobs"][job]["needs"].as_str(), Some("changes"));
        assert!(!codeql["jobs"][job]["if"].is_badvalue());
    }
    let audit = yaml(AUDIT);
    assert!(!audit["on"]["schedule"].is_badvalue());
    assert!(audit["jobs"]["audit"]["steps"]
        .as_vec()
        .expect("audit steps")
        .iter()
        .any(|step| step["id"].as_str() == Some("changes")));
}
