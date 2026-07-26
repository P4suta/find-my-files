use serde_json::Value;
use std::fs;
use std::path::Path;

#[test]
fn release_pr_guards_match_the_configured_component_branch() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let config: Value = serde_json::from_str(
        &fs::read_to_string(repo.join("release-please-config.json"))
            .expect("release-please config must be readable"),
    )
    .expect("release-please config must be valid JSON");
    let component = config["packages"]["."]["package-name"]
        .as_str()
        .expect("root package must have a package-name");
    assert_eq!(
        config["packages"]["."]["bump-minor-pre-major"].as_bool(),
        Some(true),
        "breaking changes before 1.0 must remain minor releases"
    );
    let expected = format!("release-please--branches--main--components--{component}");

    for relative in [
        ".github/workflows/release-please.yml",
        ".github/workflows/no-automerge-on-release-pr.yml",
        ".github/workflows/release-label-guard.yml",
        ".github/workflows/release-gate.yml",
    ] {
        let contents = fs::read_to_string(repo.join(relative))
            .unwrap_or_else(|error| panic!("{relative} must be readable: {error}"));
        assert!(
            contents.contains(&expected),
            "{relative} must guard the configured Release Please branch {expected}"
        );
    }
}

#[test]
fn artifact_producers_are_safe_to_rerun() {
    let workflows = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent")
        .join(".github/workflows");

    for entry in fs::read_dir(workflows).expect("workflow directory must be readable") {
        let path = entry.expect("workflow entry must be readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("yml") {
            continue;
        }
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        let lines: Vec<_> = contents.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("uses: actions/upload-artifact@") {
                continue;
            }
            let step = lines[index + 1..].iter().take_while(|candidate| {
                let trimmed = candidate.trim_start();
                !trimmed.starts_with("- name:") && !trimmed.starts_with("- uses:")
            });
            assert!(
                step.into_iter()
                    .any(|candidate| candidate.trim() == "overwrite: true"),
                "{}:{} must set overwrite: true for deterministic job reruns",
                path.display(),
                index + 1
            );
        }
    }
}

#[test]
fn release_publication_is_bound_to_exact_performance_evidence() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let dispatcher = fs::read_to_string(repo.join(".github/workflows/performance-release.yml"))
        .expect("performance dispatcher must be readable");
    let release = fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow must be readable");
    let gate = fs::read_to_string(repo.join(".github/workflows/performance-gate.yml"))
        .expect("performance gate must be readable");

    assert!(dispatcher.contains(r#"-f performance_run_id="$SOURCE_RUN_ID""#));
    assert!(dispatcher.contains("performance-gate\\ (v[0-9]+\\.[0-9]+\\.[0-9]+)"));
    assert!(dispatcher.contains("'.head_branch'"));
    assert!(release.contains("performance_run_id:"));
    assert!(release.contains("actions/runs/$env:PERFORMANCE_RUN_ID"));
    assert!(release.contains("$run.display_title -cne \"performance-gate $env:TAG\""));
    assert!(release.contains("permission-administration: read"));
    assert!(release.contains("permission-workflows: write"));
    assert!(release.contains("repos/$env:REPO/immutable-releases"));
    for (offset, _) in release.match_indices("if ($candidate.draft -and") {
        let check = release[offset..]
            .lines()
            .take(5)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(check.contains("-not $candidate.prerelease -and"));
        assert!(check.contains("$candidate.name -ceq $env:TAG"));
    }
    assert!(release.contains("--prerelease=false"));
    assert!(release.contains("stable-publication"));
    assert!(release.contains("Refusing out-of-order stable publication"));
    assert!(gate.contains("CANONICAL_BASELINE_DIR: ${{ vars.FMF_PERF_BASELINE_DIR }}"));
    assert!(gate.contains("FMF_PERF_BASELINE_DIR=$scratch"));
    assert!(gate.contains("clean: true"));
}
