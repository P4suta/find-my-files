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
        .find(|step| step["with"]["toolchain"].as_str() == Some(pin))
        .expect("CI uses the pinned Miri toolchain");
    let installed: BTreeSet<_> = toolchain["with"]["components"]
        .as_str()
        .expect("installed components")
        .split(',')
        .map(str::trim)
        .collect();
    assert_eq!(components, installed);
    assert_eq!(
        toolchain["if"].as_str(),
        Some("needs.changes.outputs.assurance == 'true'")
    );
}
