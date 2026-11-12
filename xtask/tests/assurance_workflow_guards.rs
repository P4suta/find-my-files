use std::collections::BTreeSet;

use yaml_rust2::{Yaml, YamlLoader};

fn ci() -> Yaml {
    let mut documents = YamlLoader::load_from_str(include_str!("../../.github/workflows/ci.yml"))
        .expect("valid CI workflow");
    assert_eq!(documents.len(), 1);
    documents.remove(0)
}

#[test]
fn applicable_assurance_failures_block_the_required_status() {
    let ci = ci();
    assert!(ci["jobs"]["ci-required"]["needs"]
        .as_vec()
        .expect("required dependencies")
        .contains(&Yaml::String("xtask".into())));
    let job = &ci["jobs"]["xtask"];
    assert!(job["continue-on-error"].is_badvalue());
    let steps = job["steps"].as_vec().expect("xtask steps");
    for name in [
        "Install the checksum-verified prover",
        "Verify the registered production harnesses",
        "Interpret the codec boundary tests",
    ] {
        let step = steps
            .iter()
            .find(|step| step["name"].as_str() == Some(name))
            .expect(name);
        assert_eq!(
            step["if"].as_str(),
            Some("needs.changes.outputs.assurance == 'true'")
        );
        assert!(step["continue-on-error"].is_badvalue());
    }
}

#[test]
fn miri_pin_matches_the_native_configuration() {
    let ci = ci();
    let configuration: toml_edit::DocumentMut = include_str!("../../verification/miri/mise.toml")
        .parse()
        .expect("Miri configuration");
    let pin = configuration["tools"]["rust"]["version"]
        .as_str()
        .expect("Miri pin");
    let components: BTreeSet<_> = configuration["tools"]["rust"]["components"]
        .as_array()
        .expect("Miri components")
        .iter()
        .map(|c| c.as_str().expect("component"))
        .collect();
    let toolchain = ci["jobs"]["xtask"]["steps"]
        .as_vec()
        .expect("xtask steps")
        .iter()
        .find(|step| step["name"].as_str() == Some("Install the pinned interpreter toolchain"))
        .expect("CI uses the pinned Miri toolchain");
    let mut expected = format!("rustup toolchain install {pin} --profile minimal");
    for component in components {
        expected.push_str(" --component ");
        expected.push_str(component);
    }
    assert_eq!(toolchain["run"].as_str(), Some(expected.as_str()));
    assert_eq!(
        toolchain["if"].as_str(),
        Some("needs.changes.outputs.assurance == 'true'")
    );
}

#[test]
fn full_assurance_requires_an_explicit_candidate_call() {
    let ci = ci();
    assert!(ci["on"]["push"].is_badvalue());
    assert!(ci["on"]["schedule"].is_badvalue());
    assert_eq!(
        ci["on"]["merge_group"]["types"].as_vec(),
        Some(&vec![Yaml::String("checks_requested".into())])
    );
    let inputs = &ci["on"]["workflow_call"]["inputs"];
    assert_eq!(inputs["full_assurance"]["type"].as_str(), Some("boolean"));
    assert_eq!(inputs["full_assurance"]["default"].as_bool(), Some(false));
    assert_eq!(inputs["target_sha"]["required"].as_bool(), Some(true));
    for output in ["assurance", "fuzz"] {
        assert_eq!(
            ci["jobs"]["changes"]["outputs"][output].as_str(),
            Some("${{ inputs.full_assurance || false }}")
        );
    }
    assert_eq!(
        ci["jobs"]["changes"]["outputs"]["fuzz_surfaces"].as_str(),
        Some("[\"fuzz_all\"]")
    );
}

#[test]
fn every_candidate_checkout_uses_the_admitted_commit() {
    let ci = ci();
    let mut checkouts = 0;
    for job in ci["jobs"].as_hash().expect("CI jobs").values() {
        for step in job["steps"].as_vec().into_iter().flatten() {
            if step["uses"]
                .as_str()
                .is_some_and(|action| action.starts_with("actions/checkout@"))
            {
                checkouts += 1;
                assert_eq!(
                    step["with"]["ref"].as_str(),
                    Some("${{ inputs.target_sha || github.sha }}")
                );
            }
        }
    }
    assert!(checkouts > 0);
    assert_eq!(
        ci["jobs"]["fuzz"]["with"]["target_sha"].as_str(),
        Some("${{ inputs.target_sha || github.sha }}")
    );
    let fuzz = YamlLoader::load_from_str(include_str!("../../.github/workflows/fuzz.yml"))
        .expect("valid fuzz workflow");
    let checkout = &fuzz[0]["jobs"]["fuzz"]["steps"][0];
    assert!(checkout["uses"]
        .as_str()
        .is_some_and(|action| action.starts_with("actions/checkout@")));
    assert_eq!(
        checkout["with"]["ref"].as_str(),
        Some("${{ inputs.target_sha || github.sha }}")
    );
}

#[test]
fn release_signing_requires_successful_full_assurance() {
    let release = YamlLoader::load_from_str(include_str!("../../.github/workflows/release.yml"))
        .expect("valid release workflow");
    let jobs = &release[0]["jobs"];
    let assurance = &jobs["assurance"];
    assert_eq!(assurance["needs"].as_str(), Some("preflight"));
    assert_eq!(
        assurance["uses"].as_str(),
        Some("$/.github/workflows/ci.yml")
    );
    assert_eq!(assurance["with"]["full_assurance"].as_bool(), Some(true));
    assert_eq!(
        assurance["with"]["target_sha"].as_str(),
        Some("${{ inputs.commit_sha }}")
    );
    assert!(assurance["if"].is_badvalue());
    assert!(assurance["continue-on-error"].is_badvalue());
    assert!(jobs["sign-stage"]["needs"]
        .as_vec()
        .expect("signing dependencies")
        .contains(&Yaml::String("assurance".into())));
    assert!(jobs["sign-stage"]["if"].is_badvalue());
}
