use serde_json::Value;
use std::fs;
use std::path::Path;

fn workflow_job<'a>(workflow: &'a str, name: &str, next_name: &str) -> &'a str {
    let start_marker = format!("\n  {name}:");
    let end_marker = format!("\n  {next_name}:");
    let start = workflow
        .find(&start_marker)
        .unwrap_or_else(|| panic!("workflow job {name} must exist"));
    let end = workflow[start + start_marker.len()..]
        .find(&end_marker)
        .map_or_else(
            || panic!("workflow job {next_name} must follow {name}"),
            |offset| start + start_marker.len() + offset,
        );
    &workflow[start..end]
}

fn assert_ordered(section: &str, needles: &[&str]) {
    let mut previous = None;
    for needle in needles {
        let position = section
            .find(needle)
            .unwrap_or_else(|| panic!("workflow section must contain `{needle}`"));
        if let Some((prior_name, prior_position)) = previous {
            assert!(
                prior_position < position,
                "`{prior_name}` must precede `{needle}`"
            );
        }
        previous = Some((*needle, position));
    }
}

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

/// ADR-0048 replaced the four-stage `workflow_dispatch` -> `workflow_run` ->
/// `workflow_run` -> `workflow_call` chain with one dispatch from protected
/// main. The property the chain bought — a tag identifies build data, never
/// workflow code — now rests on three things this test pins: the entry point is
/// a dispatch and nothing else, `preflight` binds that dispatch before any other
/// job runs, and the orchestrator's dedupe title is character-identical to the
/// `run-name` release.yml renders.
#[test]
fn release_publication_is_directly_dispatched_and_exactly_bound() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let orchestrator = fs::read_to_string(repo.join(".github/workflows/release-please.yml"))
        .expect("release orchestrator must be readable");
    let release = fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow must be readable");

    // The entry point, and the absence of everything the chain contributed.
    assert!(release.contains("workflow_dispatch:"));
    assert!(
        !release.contains("workflow_call:"),
        "release.yml is dispatched directly; a reusable entry point would restore the indirection"
    );
    assert!(!release.contains("performance_run_id"));
    assert!(!release.contains("performanceRunId"));
    assert!(!release.contains("performance-controller"));
    assert!(
        !release.contains("inputs.publish"),
        "the dispatched workflow has exactly one mode and no publish toggle"
    );
    assert!(!release.contains("Stamp rehearsal"));
    assert!(!release.contains("Pass through unsigned rehearsal"));
    assert!(release.contains("tag_name:"));
    assert!(release.contains("commit_sha:"));
    assert!(release.contains("release_id:"));

    // Admission control runs first and gates both long jobs.
    let preflight = release
        .find("\n  preflight:")
        .expect("release.yml must open with the admission-control job");
    let build = release
        .find("\n  build:")
        .expect("release.yml must have a build job");
    assert!(
        preflight < build,
        "preflight must be declared before the job it admits"
    );
    assert_eq!(
        release.matches("needs: preflight").count(),
        2,
        "both build and the 16-shard mutation gate must wait on admission control"
    );
    assert!(release.contains("${{ github.triggering_actor }}"));
    assert!(release.contains("GITHUB_EVENT_NAME"));
    assert!(release.contains("$env:GITHUB_EVENT_NAME -cne \"workflow_dispatch\""));
    assert_eq!(
        release
            .matches("$env:GITHUB_REF -cne \"refs/heads/main\"")
            .count(),
        6,
        "preflight plus every later job must independently refuse a non-main controller ref"
    );

    // The dedupe contract: one string, pinned from both sides.
    assert!(release.contains(
        "run-name: release publish ${{ inputs.tag_name }} ${{ inputs.commit_sha }} release ${{ inputs.release_id }}"
    ));
    assert!(orchestrator
        .contains("release_title=\"release publish $TAG $tag_sha release $release_id\""));
    assert!(orchestrator.contains("--workflow release.yml"));
    assert!(orchestrator.contains("--event workflow_dispatch"));
    assert!(orchestrator.contains("gh workflow run release.yml --repo \"$REPO\" --ref main"));
    assert!(orchestrator
        .contains("-f tag_name=\"$TAG\" -f commit_sha=\"$tag_sha\" -f release_id=\"$release_id\""));
    assert!(
        !orchestrator.contains("performance-gate-request"),
        "the retired request workflow must not be dispatched or queried"
    );

    assert_eq!(
        release.matches("ref: ${{ inputs.commit_sha }}").count(),
        4,
        "direct release jobs that execute target repository code must check out the authorized SHA; mutation uses the separately guarded reusable controller"
    );
    assert!(release.contains("uses: ./.github/workflows/mutation-controller.yml"));
    assert!(release.contains("target_sha: ${{ inputs.commit_sha }}"));
    assert!(release.contains("controller_sha: ${{ github.workflow_sha }}"));
    assert!(release.contains("permission-administration: read"));
    assert!(
        !release.contains("permission-workflows:"),
        "release publication never edits workflow files and must not mint that authority"
    );
    assert!(
        !orchestrator.contains("permission-workflows:"),
        "release-please does not edit workflow files and must retain least privilege"
    );
    assert_eq!(
        orchestrator
            .matches("actions: write # dispatch the trusted-main release workflow")
            .count(),
        1,
        "the isolated trusted-main dispatcher is the sole Actions write boundary"
    );
    assert!(release.contains("repos/$env:REPO/immutable-releases"));
    assert!(release.contains("-F prerelease=false"));
    assert!(release.contains("stable-publication"));
    assert!(release.contains("Refusing out-of-order stable publication"));
    assert!(release.contains("publish-approval:"));
    assert!(release.contains("environment: release-please"));
    assert!(!release.contains("The two repository-level App credentials"));
    assert!(!release.contains("\n    secrets:"));
    assert!(!release.contains("secrets: inherit"));
    for secret in [
        "secrets.ES_USERNAME",
        "secrets.ES_PASSWORD",
        "secrets.CREDENTIAL_ID",
        "secrets.ES_TOTP_SECRET",
    ] {
        assert_eq!(
            release.matches(secret).count(),
            1,
            "{secret} must exist in exactly one credential island"
        );
    }
    for secret in [
        "secrets.RELEASE_PLEASE_CLIENT_ID",
        "secrets.RELEASE_PLEASE_PRIVATE_KEY",
    ] {
        assert_eq!(
            release.matches(secret).count(),
            2,
            "{secret} must be referenced only by the read-only and write-only token mints"
        );
    }
    let settings_mint = release
        .find("id: immutable-settings-token")
        .expect("settings token mint must exist");
    let publication_mint = release
        .find("id: publication-token")
        .expect("publication token mint must exist");
    let attestations = release
        .rfind("name: Attest C# SBOM")
        .expect("attestations must exist");
    assert!(settings_mint < attestations);
    assert!(publication_mint > attestations);

    // The alerts this change removes were structural: CodeQL's cache-poisoning
    // query binds `workflow_run` through `runsOnDefaultBranch`, and zizmor's
    // dangerous-triggers audit had to be suppressed wherever we used one. Both
    // constructs are gone from the release path, so keep them gone repo-wide
    // rather than re-earning the alerts one workflow at a time.
    let mut dangerous_trigger_suppressions = Vec::new();
    for entry in
        fs::read_dir(repo.join(".github/workflows")).expect("workflow directory must be readable")
    {
        let path = entry.expect("workflow entry must be readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("yml") {
            continue;
        }
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        assert!(
            !contents.contains("\n  workflow_run:"),
            "{} reintroduces a workflow_run trigger (ADR-0048)",
            path.display()
        );
        if contents.contains("zizmor: ignore[dangerous-triggers]") {
            dangerous_trigger_suppressions.push(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .expect("workflow file name")
                    .to_owned(),
            );
        }
    }
    // The one remaining suppression is the `pull_request_target` auto-merge
    // guard, which never checks out PR code and is unrelated to the release
    // path. Nothing on the release path may need one.
    assert_eq!(
        dangerous_trigger_suppressions,
        vec!["no-automerge-on-release-pr.yml".to_owned()],
        "a dangerous trigger was suppressed instead of avoided"
    );
}

#[test]
fn release_identity_signing_and_attestation_boundaries_are_fail_closed() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let orchestrator = fs::read_to_string(repo.join(".github/workflows/release-please.yml"))
        .expect("release orchestrator must be readable");
    let release = fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow must be readable");

    for boundary in [
        "upload_url: ${{ steps.rp.outputs.upload_url }}",
        "upload_prefix=\"https://uploads.github.com/repos/$REPO/releases/\"",
        "release_json=$(gh api \"repos/$REPO/releases/$release_id\")",
        "-f release_id=\"$release_id\"",
    ] {
        assert!(
            orchestrator.contains(boundary),
            "release-please must preserve exact release identity boundary `{boundary}`"
        );
    }

    let publish = release
        .split_once("\n  publish:")
        .expect("publish job must exist")
        .1;
    assert!(!release.contains("gh release upload"));
    assert!(!release.contains("gh release edit"));
    assert!(!release.contains("releases/tags/"));
    assert!(!publish.contains("--method DELETE"));
    assert_eq!(publish.matches("gh api --method PATCH").count(), 1);
    assert_eq!(
        publish
            .matches("gh api --hostname uploads.github.com --method POST")
            .count(),
        1
    );
    assert_eq!(
        publish.matches("releases?per_page=100").count(),
        1,
        "release listing is allowed once only for monotonic-version policy"
    );
    for boundary in [
        "repos/$env:REPO/releases/$env:RELEASE_ID",
        "repos/$env:REPO/releases/$env:RELEASE_ID/assets?name=$name",
        "gh api --method PATCH",
        "Existing draft asset '$($asset.name)' is not byte-identical; refusing replacement.",
    ] {
        assert!(
            publish.contains(boundary),
            "publication must retain exact-ID boundary `{boundary}`"
        );
    }

    assert!(release.contains("echo \"FMF_SOURCE_COMMIT=$TARGET_SHA\""));
    assert!(release.contains("BUILDINFO must contain exactly one commit field."));
    assert!(!release.contains("actions/attest-build-provenance@"));
    for boundary in [
        "name: Generate and verify exact release predicate",
        "predicate-type: https://github.com/P4suta/find-my-files/attestations/release/v1",
        "\"schemaVersion\": 2",
        "\"triggeringActor\": triggering_actor",
        "\"workflowCommit\": workflow_sha",
        "\"bundleManifests\": manifest_records",
        "document[\"source_commit\"] != target_sha",
        "actual_paths != expected_paths",
    ] {
        assert!(
            publish.contains(boundary),
            "custom release attestation must retain `{boundary}`"
        );
    }
    assert!(!publish.contains("find-my-files-*-win-x64.zip"));
    assert_ordered(
        publish,
        &[
            "name: Generate and verify exact release predicate",
            "name: Attest exact release identity",
            "name: Attest Rust SBOM",
            "name: Attest C# SBOM",
            "id: publication-token",
            "Repository immutable releases were disabled before publication.",
            "Draft release identity changed immediately before publication.",
            "Pre-publication asset '$($asset.name)' differs from the approved candidate.",
            "$published = gh api --method PATCH",
            "name: Verify the published release is immutable and complete",
        ],
    );
}

#[test]
fn release_artifacts_are_exactly_sealed_across_every_handoff() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let release = fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow must be readable");
    let scanner = fs::read_to_string(repo.join(".github/actions/sbom-scan/action.yml"))
        .expect("SBOM scanner action must be readable");

    let build = workflow_job(&release, "build", "sbom");
    assert!(build.contains("version: ${{ steps.release-version.outputs.version }}"));
    assert!(build.contains("run: just verify-release-source"));
    assert!(!build.contains("setup-actionlint"));
    assert_ordered(
        build,
        &[
            "name: Build distributable bundle",
            "name: Seal and verify exact unsigned bundle",
            "name: Require pristine unsigned first-party binaries",
            "name: Reverify unsigned bundle after runtime tests",
            "name: Upload sealed unsigned bundle",
        ],
    );
    assert_eq!(build.matches("just bundle-verify unsigned").count(), 2);
    for exact_input in [
        "build/dist/FindMyFiles\n",
        "build/dist/FindMyFiles.unsigned.manifest.json",
        "include-hidden-files: true",
    ] {
        assert!(
            build.contains(exact_input),
            "unsigned artifact must include exact sealed input `{exact_input}`"
        );
    }

    let sbom = workflow_job(&release, "sbom", "sign-stage");
    assert!(sbom.contains("needs: build"));
    assert!(sbom.contains("FMF_BUILD_VERSION: ${{ needs.build.outputs.version }}"));
    assert!(sbom.contains("ref: ${{ inputs.commit_sha }}"));
    assert!(sbom.contains("path: build/dist"));
    assert!(!sbom.contains("just publish"));
    assert_ordered(
        sbom,
        &[
            "name: Download sealed unsigned bundle",
            "name: Verify exact unsigned SBOM source artifact",
            "name: Restore locked NuGet SBOM evidence",
            "uses: ./.github/actions/sbom-scan",
            "name: Reverify source artifact and canonical SBOM pair",
            "name: Upload exact SBOM pair",
        ],
    );
    assert_eq!(sbom.matches("just bundle-verify unsigned").count(), 2);
    for exact_bom in ["build/sbom/fmf-engine.cdx.json", "build/sbom/app.cdx.json"] {
        assert!(
            sbom.contains(exact_bom),
            "SBOM artifact must explicitly upload `{exact_bom}`"
        );
    }

    let sign_stage = workflow_job(&release, "sign-stage", "sign");
    assert!(sign_stage.contains("needs: [build, sbom, mutation]"));
    assert!(sign_stage.contains("path: build/dist"));
    assert!(!sign_stage.contains("actions/checkout@"));
    assert!(!sign_stage.contains("./.github/actions/"));
    assert!(!sign_stage.contains("just "));
    for boundary in [
        "\"FindMyFiles.exe\" = \"FindMyFiles.exe\"",
        "\"app-FindMyFiles.exe\" = \"app/FindMyFiles.exe\"",
        "\"app-FindMyFiles.dll\" = \"app/FindMyFiles.dll\"",
        "\"fmf-service.exe\" = \"app/fmf-service.exe\"",
        "\"fmf_engine.dll\" = \"app/fmf_engine.dll\"",
        "Get-AuthenticodeSignature -FilePath $sourcePath",
        "[IO.FileAttributes]::ReparsePoint",
        "Get-FileHash -LiteralPath $stagePath -Algorithm SHA256",
        "Signing input does not match the exact five-name allowlist.",
    ] {
        assert!(
            sign_stage.contains(boundary),
            "trusted sign staging must retain `{boundary}`"
        );
    }
    assert_ordered(
        sign_stage,
        &[
            "name: Download sealed unsigned bundle",
            "name: Revalidate draft identity before staging",
            "name: Materialize exact credential-boundary signing input",
            "name: Upload flat signing input",
        ],
    );
    for signing_input in [
        "build/sign-stage/FindMyFiles.exe",
        "build/sign-stage/app-FindMyFiles.exe",
        "build/sign-stage/app-FindMyFiles.dll",
        "build/sign-stage/fmf-service.exe",
        "build/sign-stage/fmf_engine.dll",
    ] {
        assert_eq!(
            sign_stage.matches(signing_input).count(),
            1,
            "sign-stage upload must literally enumerate `{signing_input}` once"
        );
    }

    let sign = workflow_job(&release, "sign", "sign-collect");
    assert!(sign.contains("permissions: {}"));
    assert!(!sign.contains("actions/checkout@"));
    assert!(!sign.contains("./.github/actions/"));
    assert!(!sign
        .lines()
        .any(|line| line.trim_start().starts_with("run:")));
    assert!(!sign
        .lines()
        .any(|line| line.trim_start().starts_with("shell:")));
    let sign_actions = sign
        .lines()
        .filter_map(|line| line.trim().strip_prefix("uses: "))
        .collect::<Vec<_>>();
    assert_eq!(sign_actions.len(), 4);
    assert!(sign_actions.iter().all(|action| {
        action.starts_with("actions/download-artifact@")
            || action.starts_with("actions/upload-artifact@")
            || action.starts_with("SSLcom/esigner-codesign@")
    }));
    for signed_output in [
        "build/signed/FindMyFiles.exe",
        "build/signed/app-FindMyFiles.exe",
        "build/signed/app-FindMyFiles.dll",
        "build/signed/fmf-service.exe",
        "build/signed/fmf_engine.dll",
    ] {
        assert_eq!(
            sign.matches(signed_output).count(),
            1,
            "credential island must upload literal signed output `{signed_output}` once"
        );
    }

    let sign_collect = workflow_job(&release, "sign-collect", "package");
    assert!(sign_collect.contains("path: build/dist"));
    assert_ordered(
        sign_collect,
        &[
            "name: Require the exact signed-result file set",
            "uses: ./.github/actions/rust-toolchain",
            "name: Verify exact unsigned bundle before collection",
            "name: Copy signed binaries back into the bundle",
            "name: Verify signing-only bundle transition",
            "name: Verify signatures",
            "name: Seal and verify exact signed bundle",
            "name: Upload sealed release-candidate bundle",
        ],
    );
    let result_check = sign_collect
        .find("name: Require the exact signed-result file set")
        .expect("signed-result check must exist");
    let local_action = sign_collect
        .find("uses: ./.github/actions/rust-toolchain")
        .expect("target-local setup must exist");
    assert!(
        result_check < local_action,
        "signed-result allowlist must run before target-controlled local Action"
    );
    assert!(sign_collect.contains("Signed-result artifact must contain exactly five flat files."));
    assert!(sign_collect.contains("[IO.FileAttributes]::ReparsePoint"));
    assert_eq!(
        sign_collect
            .matches("just bundle-verify-signed-transition")
            .count(),
        1
    );
    for manifest in [
        "build/dist/FindMyFiles.unsigned.manifest.json",
        "build/dist/FindMyFiles.signed.manifest.json",
    ] {
        assert!(
            sign_collect.contains(manifest),
            "signed candidate must carry `{manifest}`"
        );
    }

    let package = workflow_job(&release, "package", "publish-approval");
    assert!(package.contains("needs: [build, sbom, sign-collect]"));
    assert!(package.contains("FMF_BUILD_VERSION: ${{ needs.build.outputs.version }}"));
    assert!(package.contains("path: build/dist"));
    assert_ordered(
        package,
        &[
            "name: Download release-candidate bundle",
            "name: Verify exact signed bundle handoff",
            "name: Require the bundle to be signed before publishing",
            "name: Download SBOMs",
            "name: Verify exact SBOM handoff",
            "name: Package (zip + checksums)",
            "name: Reverify sealed inputs after packaging",
            "name: Upload immutable release assets",
        ],
    );
    assert_eq!(package.matches("just bundle-verify signed").count(), 2);
    assert_eq!(
        package
            .matches("just sbom-verify $env:FMF_BUILD_VERSION")
            .count(),
        2
    );
    for manifest in [
        "build/dist/FindMyFiles.unsigned.manifest.json",
        "build/dist/FindMyFiles.signed.manifest.json",
    ] {
        assert!(
            package.contains(manifest),
            "release-assets handoff must include `{manifest}` for source attestation"
        );
    }

    assert_eq!(
        scanner
            .matches("just sbom-verify $env:FMF_BUILD_VERSION")
            .count(),
        2
    );
    assert!(scanner.contains("Copy-Item -LiteralPath build/sbom/fmf-engine.cdx.json"));
    assert!(scanner.contains("Copy-Item -LiteralPath build/sbom/app.cdx.json"));
    let scan_step = scanner
        .split_once("- name: Scan SBOMs for known vulnerabilities (gate)")
        .expect("scanner step must exist")
        .1
        .split_once("- name: Verify the canonical SBOM pair after scanning")
        .expect("post-scan verification must exist")
        .0;
    assert!(scan_step.contains("Join-Path $scanDirectory"));
    assert!(
        !scan_step.contains("-L build/sbom/"),
        "the scanner may consume only disposable copies"
    );
}

#[test]
fn sbom_action_is_a_thin_xtask_adapter_for_three_raw_rust_graphs() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let action = fs::read_to_string(repo.join(".github/actions/sbom-scan/action.yml"))
        .expect("SBOM action must be readable");
    let justfile = fs::read_to_string(repo.join("justfile")).expect("justfile must be readable");
    let implementation =
        fs::read_to_string(repo.join("xtask/src/sbom.rs")).expect("xtask SBOM logic must exist");

    for boundary in [
        "--cargo-package fmf-service",
        "--cargo-package fmf-ffi",
        "--cargo-package fmf-launcher",
        "fmf-service.cdx.json",
        "fmf-ffi.cdx.json",
        "fmf-launcher.cdx.json",
        "just sbom $env:FMF_BUILD_VERSION $rawDirectory",
    ] {
        assert!(
            action.contains(boundary),
            "SBOM action must retain thin-adapter boundary `{boundary}`"
        );
    }
    for forbidden in [
        "ConvertFrom-Json",
        "ConvertTo-Json",
        "Assert-BomGraph",
        "dotnet tool install CycloneDX",
        "$env:GITHUB_PATH",
    ] {
        assert!(
            !action.contains(forbidden),
            "SBOM validation/generation must live in xtask, not the action: {forbidden}"
        );
    }
    assert!(
        justfile.contains("sbom version cargo_raw_dir:")
            && justfile.contains("--cargo-raw-dir \"{{cargo_raw_dir}}\""),
        "justfile must expose the thin xtask SBOM entry point"
    );
    for invariant in [
        "cargo-sbom 0.10 wrapper must not masquerade as a package root",
        "selected cargo root",
        "developer-only crate",
        "shipped PE",
        "ready-to-run",
        "validate_final_bom",
    ] {
        assert!(
            implementation.contains(invariant),
            "xtask SBOM implementation lost fail-closed invariant `{invariant}`"
        );
    }
}

#[test]
fn signature_identity_is_an_exact_nonblank_common_name() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let action = fs::read_to_string(repo.join(".github/actions/verify-signatures/action.yml"))
        .expect("signature verifier must be readable");
    let release = fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow must be readable");

    for boundary in [
        "signer-common-name:",
        "IsNullOrWhiteSpace($expectedCommonName)",
        "$null -eq $certificate",
        "X509NameType]::SimpleName",
        "[StringComparison]::Ordinal",
    ] {
        assert!(
            action.contains(boundary),
            "signature verifier must retain exact identity boundary {boundary}"
        );
    }
    assert!(!action.contains("-notlike"));
    assert!(!action.contains("signer-subject-contains"));
    assert!(!action.contains("SIGNER_SUBJECT_CONTAINS"));
    assert_eq!(release.matches("signer-common-name:").count(), 2);
    assert!(release.contains("SIGNER_COMMON_NAME: \"Yasunobu Sakashita\""));
}

#[test]
fn split_nextest_runs_keep_independent_junit_evidence() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent");
    let workflow = fs::read_to_string(repo.join(".github/workflows/ci.yml"))
        .expect("CI workflow must be readable");
    let config = fs::read_to_string(repo.join("engine/.config/nextest.toml"))
        .expect("engine nextest config must be readable");
    let nightly_admin = fs::read_to_string(repo.join(".github/workflows/nightly-admin.yml"))
        .expect("nightly admin workflow must be readable");
    let release = fs::read_to_string(repo.join(".github/workflows/release.yml"))
        .expect("release workflow must be readable");

    assert!(workflow.contains("--profile ci-latency"));
    assert!(workflow.contains("--profile ci-coverage"));
    assert!(workflow.contains("build/engine/nextest/ci-latency/latency.xml"));
    assert!(workflow.contains("build/engine/nextest/ci-coverage/coverage.xml"));
    assert!(config.contains("[profile.ci-latency.junit]"));
    assert!(config.contains("[profile.ci-coverage.junit]"));
    assert!(config.contains("[profile.admin.junit]"));
    assert_eq!(config.matches("path = \"latency.xml\"").count(), 1);
    assert_eq!(config.matches("path = \"coverage.xml\"").count(), 1);
    assert_eq!(config.matches("path = \"admin.xml\"").count(), 1);
    for workflow in [&nightly_admin, &release] {
        assert!(workflow.contains("run: just test-admin"));
        assert!(workflow.contains("build/engine/nextest/admin/admin.xml"));
        assert!(
            !workflow.contains("build/engine/nextest/ci/junit.xml")
                && !workflow.contains("build/engine/nextest-ci.xml"),
            "admin evidence must use the isolated nextest admin profile store"
        );
    }
}
