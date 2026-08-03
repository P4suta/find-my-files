//! Structural tripwires for the trusted exact-identity mutation gates
//! (ADR-0022).
//!
//! Runtime parsers prove report semantics. These tests pin the surrounding
//! trust split: default-branch caller -> same-commit reusable controller ->
//! separate immutable target data checkout -> fresh evidence verifier.

const MUTANTS_WORKFLOW: &str = include_str!("../../.github/workflows/mutants.yml");
const CONTROLLER_WORKFLOW: &str = include_str!("../../.github/workflows/mutation-controller.yml");
const RELEASE_WORKFLOW: &str = include_str!("../../.github/workflows/release.yml");
const JUSTFILE: &str = include_str!("../../justfile");
const STRYKER_CONFIG: &str = include_str!("../../app/FindMyFiles.Tests/stryker-config.json");
const DOTNET_TOOLS: &str = include_str!("../../.config/dotnet-tools.json");
const GITIGNORE: &str = include_str!("../../.gitignore");

// Both gates refuse to run without their reviewed baseline, so a baseline that
// is missing from the tree turns the weekly audit into a start-up crash that
// reports nothing about the code. These two `include_str!`s are the cheapest
// possible tripwire for that: delete or rename either file and this test crate
// stops compiling.
const RUST_MUTATION_BASELINE: &str = include_str!("../../engine/mutation-baseline.json");
const CSHARP_MUTATION_BASELINE: &str =
    include_str!("../../app/FindMyFiles.Tests/mutation-baseline.json");

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = text
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let (section, _) = tail
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end `{end}`"));
    section
}

#[test]
fn weekly_and_manual_audits_authorize_only_a_default_branch_controller() {
    assert!(MUTANTS_WORKFLOW.contains("  schedule:\n"));
    assert!(MUTANTS_WORKFLOW.contains("  workflow_dispatch:\n"));
    assert!(MUTANTS_WORKFLOW.contains("      target_sha:\n"));
    assert!(!MUTANTS_WORKFLOW.contains("pull_request:"));
    assert!(!MUTANTS_WORKFLOW.contains("pull_request_target:"));
    assert!(!MUTANTS_WORKFLOW.contains("continue-on-error:"));

    let authorize = between(MUTANTS_WORKFLOW, "\n  authorize:\n", "\n  mutation:\n");
    assert!(authorize.contains("CONTROLLER_REF: ${{ github.ref }}"));
    assert!(authorize.contains("CONTROLLER_SHA: ${{ github.sha }}"));
    assert!(authorize.contains("WORKFLOW_SHA: ${{ github.workflow_sha }}"));
    assert!(authorize.contains("ref !== \"refs/heads/main\""));
    assert!(authorize.contains("controller !== workflow"));
    assert!(authorize.contains("repositoryRecord.default_branch !== \"main\""));
    assert!(authorize.contains("compareCommitsWithBasehead"));
    assert!(authorize.contains("github.rest.repos.getCommit"));
    assert!(authorize.contains("String(targetCommit.sha).toLowerCase() !== target"));

    let invocation = MUTANTS_WORKFLOW
        .split_once("\n  mutation:\n")
        .expect("mutation caller job")
        .1;
    assert!(invocation.contains("uses: ./.github/workflows/mutation-controller.yml"));
    assert!(invocation.contains("actions: read # let the fresh nested verifier"));
    assert!(invocation.contains("controller_sha: ${{ needs.authorize.outputs.controller_sha }}"));
    assert!(invocation.contains("target_sha: ${{ needs.authorize.outputs.target_sha }}"));
    assert!(!invocation.contains("actions/checkout@"));
    assert!(!invocation.contains("run: just "));
}

#[test]
fn reusable_controller_uses_two_exact_checkouts_and_no_target_controller_code() {
    assert!(CONTROLLER_WORKFLOW.contains("  workflow_call:\n"));
    assert!(!CONTROLLER_WORKFLOW.contains("  workflow_dispatch:\n"));
    assert!(!CONTROLLER_WORKFLOW.contains("  schedule:\n"));
    assert!(!CONTROLLER_WORKFLOW.contains("pull_request"));
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("\n          path: controller\n")
            .count(),
        3,
        "both producers and the fresh verifier need an isolated controller checkout"
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("\n          path: target\n")
            .count(),
        3,
        "both producers and the fresh verifier need an isolated target checkout"
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("ref: ${{ inputs.controller_sha }}")
            .count(),
        3
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("ref: ${{ inputs.target_sha }}")
            .count(),
        3
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("persist-credentials: false")
            .count(),
        6
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("working-directory: controller/xtask")
            .count(),
        3
    );
    assert!(!CONTROLLER_WORKFLOW.contains("uses: ./target/"));
    assert!(!CONTROLLER_WORKFLOW.contains("working-directory: target/xtask"));
    assert!(!CONTROLLER_WORKFLOW.contains("run: just "));
    assert!(!CONTROLLER_WORKFLOW.contains("target/justfile"));
    assert!(!CONTROLLER_WORKFLOW.contains("target/xtask"));
    assert!(!CONTROLLER_WORKFLOW.contains("Swatinem/rust-cache"));
}

#[test]
fn every_controller_job_binds_the_defining_workflow_commit_and_clean_trees() {
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("WORKFLOW_SHA: ${{ github.workflow_sha }}")
            .count(),
        3
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("$env:WORKFLOW_SHA -cne $env:CONTROLLER_SHA")
            .count(),
        3
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("[IO.FileAttributes]::ReparsePoint")
            .count(),
        3
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("status --porcelain=v1 --untracked-files=all")
            .count(),
        3
    );
    assert_eq!(CONTROLLER_WORKFLOW.matches("rev-parse HEAD").count(), 3);
}

#[test]
fn pinned_tools_and_trusted_runner_interfaces_cannot_be_narrowed_by_the_target() {
    assert!(CONTROLLER_WORKFLOW.contains("cargo-mutants@27.1.0"));
    assert!(CONTROLLER_WORKFLOW.contains("cargo-nextest@0.9.140"));
    assert!(CONTROLLER_WORKFLOW.contains("dotnet-version: 10.0.302"));
    assert!(CONTROLLER_WORKFLOW.contains("controller/mise.toml"));
    assert!(CONTROLLER_WORKFLOW.contains("fallback: none"));
    assert!(CONTROLLER_WORKFLOW.contains("mutation-rust `"));
    assert!(CONTROLLER_WORKFLOW.contains("mutation-csharp `"));
    assert!(CONTROLLER_WORKFLOW.contains("--target-root $env:TARGET_ROOT"));
    assert!(CONTROLLER_WORKFLOW.contains("--target-sha $env:TARGET_SHA"));
    assert!(CONTROLLER_WORKFLOW.contains("--controller-sha $env:CONTROLLER_SHA"));

    let rust = between(JUSTFILE, "\nmutants:\n", "\n\n# Mutation testing (C#");
    let csharp = between(JUSTFILE, "\nstryker:\n", "\n\n[group('quality')]");
    assert!(rust.contains("cargo run --locked --release -- mutation-rust"));
    assert!(csharp.contains("cargo run --locked --release -- mutation-csharp"));
    assert!(!rust.contains("{{args}}"));
    assert!(!csharp.contains("{{args}}"));
    assert!(!JUSTFILE.contains("--baseline=skip"));
}

#[test]
fn all_mutants_are_partitioned_into_one_exact_disjoint_shard_set() {
    let exact_shards = "shard: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]";
    assert_eq!(CONTROLLER_WORKFLOW.matches(exact_shards).count(), 2);
    assert_eq!(CONTROLLER_WORKFLOW.matches("fail-fast: false").count(), 2);
    assert_eq!(CONTROLLER_WORKFLOW.matches("max-parallel: 8").count(), 2);
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("SHARD_INDEX: ${{ matrix.shard }}")
            .count(),
        2
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("--shard-index $env:SHARD_INDEX")
            .count(),
        2
    );
    assert_eq!(
        CONTROLLER_WORKFLOW.matches("--shard-count 16").count(),
        4,
        "both producers and both fresh verifiers must agree on the denominator"
    );
    assert_eq!(
        CONTROLLER_WORKFLOW.matches("--run-id $env:RUN_ID").count(),
        4
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("--run-attempt $env:RUN_ATTEMPT")
            .count(),
        4,
        "receipts must reject mixed artifacts from earlier rerun attempts"
    );
    assert!(CONTROLLER_WORKFLOW.contains("name: mutation-rust-raw-${{ matrix.shard }}"));
    assert!(CONTROLLER_WORKFLOW.contains("name: mutation-csharp-raw-${{ matrix.shard }}"));
    assert!(CONTROLLER_WORKFLOW
        .contains("path: controller/build/mutation/rust/shard-${{ matrix.shard }}-of-16/"));
    assert!(CONTROLLER_WORKFLOW
        .contains("path: controller/build/mutation/csharp/shard-${{ matrix.shard }}-of-16/"));
    assert!(CONTROLLER_WORKFLOW.contains("github.rest.actions.listWorkflowRunArtifacts"));
    assert!(CONTROLLER_WORKFLOW.contains("Expected ${expected.size} raw mutation artifacts"));
    assert!(CONTROLLER_WORKFLOW.contains("Unexpected or duplicate raw mutation artifact"));
    assert!(CONTROLLER_WORKFLOW.contains("reuses artifact id ${artifact.id}"));
    assert!(CONTROLLER_WORKFLOW.contains("artifact-ids: ${{ steps.artifacts.outputs.rust_ids }}"));
    assert!(CONTROLLER_WORKFLOW.contains("artifact-ids: ${{ steps.artifacts.outputs.csharp_ids }}"));
    assert_eq!(
        CONTROLLER_WORKFLOW.matches("merge-multiple: false").count(),
        2
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("digest-mismatch: error")
            .count(),
        2
    );
}

#[test]
fn fresh_job_reparses_complete_raw_evidence_without_executing_target_code() {
    let verify = CONTROLLER_WORKFLOW
        .split_once("\n  verify:\n")
        .expect("fresh verifier job")
        .1;
    assert!(verify.contains("needs: [rust, csharp]"));
    assert!(verify.contains("if: ${{ always() }}"));
    assert!(verify.contains("RUST_RESULT: ${{ needs.rust.result }}"));
    assert!(verify.contains("CSHARP_RESULT: ${{ needs.csharp.result }}"));
    assert!(verify.contains("mutation-verify-rust `"));
    assert!(verify.contains("mutation-verify-csharp `"));
    assert!(verify.contains("--evidence-root $env:RUST_EVIDENCE_ROOT"));
    assert!(verify.contains("--evidence-root $env:CSHARP_EVIDENCE_ROOT"));
    assert!(verify.contains("Check out the exact target as inert verification data"));
    assert!(!verify.contains("cargo-mutants@"));
    assert!(!verify.contains("actions/setup-dotnet@"));
    assert!(!verify.contains("dotnet "));
    assert!(!verify.contains("cargo nextest"));
    assert!(!verify.contains("cargo test"));

    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("if-no-files-found: error")
            .count(),
        3
    );
    assert_eq!(
        CONTROLLER_WORKFLOW
            .matches("include-hidden-files: true")
            .count(),
        3
    );
    assert!(CONTROLLER_WORKFLOW.contains("path: controller/build/mutation/rust/"));
    assert!(CONTROLLER_WORKFLOW.contains("path: controller/build/mutation/csharp/"));
    assert!(CONTROLLER_WORKFLOW.contains("            evidence/rust/\n"));
    assert!(CONTROLLER_WORKFLOW.contains("            evidence/csharp/\n"));
    assert!(CONTROLLER_WORKFLOW.contains("            controller/build/mutation/verified/\n"));
}

#[test]
fn mutation_boundary_is_secretless_read_only_and_hosted() {
    assert!(!CONTROLLER_WORKFLOW.contains("secrets."));
    assert!(!CONTROLLER_WORKFLOW.contains("environment:"));
    assert!(!CONTROLLER_WORKFLOW.contains("id-token:"));
    assert!(!CONTROLLER_WORKFLOW.contains("contents: write"));
    assert!(!CONTROLLER_WORKFLOW.contains("actions: write"));
    assert!(!CONTROLLER_WORKFLOW.contains("continue-on-error:"));
    assert!(!CONTROLLER_WORKFLOW.contains("self-hosted"));
    assert_eq!(
        CONTROLLER_WORKFLOW.matches("runs-on: windows-2025").count(),
        3
    );
}

#[test]
fn stryker_json_report_is_required_instead_of_a_score_only_reporter() {
    let config: serde_json::Value =
        serde_json::from_str(STRYKER_CONFIG).expect("stryker-config.json must be valid JSON");
    let root = config
        .get("stryker-config")
        .and_then(serde_json::Value::as_object)
        .expect("stryker-config root");
    assert_eq!(
        root.get("report-file-name")
            .and_then(serde_json::Value::as_str),
        Some("mutation-report")
    );
    assert_eq!(
        root.get("reporters"),
        Some(&serde_json::json!(["progress", "json"]))
    );
}

#[test]
fn local_stryker_run_has_enough_time_for_integration_tests() {
    let config: serde_json::Value =
        serde_json::from_str(STRYKER_CONFIG).expect("stryker-config.json must be valid JSON");
    assert_eq!(
        config.pointer("/stryker-config/additional-timeout"),
        Some(&serde_json::json!(30_000))
    );
}

/// The four files that define what the mutation gates review — two scopes and
/// the two reviewed baselines they are compared against — are gate *inputs*,
/// not gate output. A baseline that is absent (never added to git, or hidden by
/// an ignore rule) does not weaken the gate gradually: `read_baseline` fails on
/// the first read, every scheduled run dies before testing a single mutant, and
/// the report says nothing about the code. That is exactly how these two
/// baselines sat unnoticed. `include_str!` above pins their presence at compile
/// time; this test pins that they stay visible to git and keep declaring the
/// same tool the run is pinned to.
#[test]
fn both_mutation_baselines_stay_present_visible_to_git_and_tool_pinned() {
    let rust: serde_json::Value = serde_json::from_str(RUST_MUTATION_BASELINE)
        .expect("engine/mutation-baseline.json must be valid JSON");
    let csharp: serde_json::Value = serde_json::from_str(CSHARP_MUTATION_BASELINE)
        .expect("app/FindMyFiles.Tests/mutation-baseline.json must be valid JSON");

    for (label, baseline, expected_tool) in [
        ("engine", &rust, "cargo-mutants"),
        ("app", &csharp, "dotnet-stryker"),
    ] {
        assert_eq!(
            baseline
                .get("schema_version")
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "{label} baseline schema_version"
        );
        assert_eq!(
            baseline
                .pointer("/tool/name")
                .and_then(serde_json::Value::as_str),
            Some(expected_tool),
            "{label} baseline tool name"
        );
        let examined = baseline
            .get("examined_files")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{label} baseline needs an examined_files array"));
        assert!(
            !examined.is_empty(),
            "{label} baseline examined_files must name the reviewed scope"
        );
        assert!(
            baseline
                .get("accepted_equivalents")
                .is_some_and(serde_json::Value::is_array),
            "{label} baseline needs an accepted_equivalents array"
        );
    }

    // The reviewed survivor set is only meaningful for the tool that produced
    // it, so the baselines' pins and the versions the run actually installs are
    // one fact in three files.
    let cargo_mutants = rust
        .pointer("/tool/version")
        .and_then(serde_json::Value::as_str)
        .expect("engine baseline tool version");
    assert!(
        CONTROLLER_WORKFLOW.contains(&format!("cargo-mutants@{cargo_mutants}")),
        "the controller installs a different cargo-mutants than the Rust baseline pins"
    );
    let stryker = csharp
        .pointer("/tool/version")
        .and_then(serde_json::Value::as_str)
        .expect("app baseline tool version");
    assert!(
        DOTNET_TOOLS.contains(&format!("\"version\": \"{stryker}\"")),
        ".config/dotnet-tools.json pins a different Stryker.NET than the C# baseline"
    );

    for input in [
        "engine/mutants.toml",
        "engine/mutation-baseline.json",
        "app/FindMyFiles.Tests/stryker-config.json",
        "app/FindMyFiles.Tests/mutation-baseline.json",
    ] {
        if let Some(pattern) = gitignore_pattern_hiding(input) {
            panic!("`{input}` is a mutation-gate input but .gitignore hides it via `{pattern}`");
        }
    }
}

/// The first `.gitignore` pattern that would keep `path` out of a fresh clone,
/// if any. Models the subset of the format this repository uses: comments,
/// negations, anchored patterns (any pattern containing a non-trailing `/`),
/// directory patterns (trailing `/`), `*` within a path segment and `**`
/// across segments. Unmodelled syntax would only ever make this over-report,
/// which is a loud failure rather than a silent hole.
fn gitignore_pattern_hiding(path: &str) -> Option<&'static str> {
    GITIGNORE.lines().find(|line| {
        let pattern = line.trim();
        if pattern.is_empty() || pattern.starts_with('#') || pattern.starts_with('!') {
            return false;
        }
        let body = pattern.trim_end_matches('/');
        let anchored = body.contains('/');
        let body = body.trim_start_matches('/');
        let segments: Vec<&str> = path.split('/').collect();
        if anchored {
            (1..=segments.len()).any(|count| wildcard_matches(body, &segments[..count].join("/")))
        } else {
            segments
                .iter()
                .any(|segment| wildcard_matches(body, segment))
        }
    })
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    match pattern.find('*') {
        None => pattern == text,
        Some(index) => {
            if !text.starts_with(&pattern[..index]) {
                return false;
            }
            let rest = &text[index..];
            let (crosses, tail) = pattern[index..]
                .strip_prefix("**")
                .map_or_else(|| (false, &pattern[index + 1..]), |after| (true, after));
            (0..=rest.len())
                .filter(|split| rest.is_char_boundary(*split))
                .filter(|split| crosses || !rest[..*split].contains('/'))
                .any(|split| wildcard_matches(tail, &rest[split..]))
        }
    }
}

#[test]
fn gitignore_matching_models_the_patterns_this_repository_uses() {
    assert!(wildcard_matches("build", "build"));
    assert!(wildcard_matches("*.user", "settings.user"));
    assert!(!wildcard_matches("*.user", "settings.json"));
    assert!(wildcard_matches("app/**/obj", "app/FindMyFiles/obj"));
    assert!(!wildcard_matches("app/*/obj", "app/a/b/obj"));
    assert!(gitignore_pattern_hiding("build/mutation/rust/gate.json").is_some());
    assert!(gitignore_pattern_hiding("engine/mutants.toml").is_none());
}

#[test]
fn release_uses_the_same_trusted_controller_and_blocks_signing_on_it() {
    let mutation = between(
        RELEASE_WORKFLOW,
        "\n  mutation:\n",
        "\n  # ---------------------------------------------------------------------------\n  # 1c)",
    );
    // The gate is 16 shards behind a 360-minute timeout, and since ADR-0048 a
    // release is startable by anyone holding Actions:write. It must not begin
    // until the secretless preflight has admitted the dispatch.
    assert!(mutation.contains("needs: preflight"));
    assert!(mutation.contains("actions: read # let the fresh nested verifier"));
    assert!(mutation.contains("contents: read"));
    assert!(mutation.contains("uses: ./.github/workflows/mutation-controller.yml"));
    assert!(mutation.contains("controller_sha: ${{ github.workflow_sha }}"));
    assert!(mutation.contains("target_sha: ${{ inputs.commit_sha }}"));
    assert!(!mutation.contains("actions/checkout@"));
    assert!(!mutation.contains("run:"));
    assert!(!mutation.contains("secrets."));
    assert!(!mutation.contains("environment:"));
    assert!(!mutation.contains("id-token:"));
    assert!(!mutation.contains("contents: write"));
    assert!(!mutation.contains("continue-on-error:"));

    let sign_stage = between(
        RELEASE_WORKFLOW,
        "\n  sign-stage:\n",
        "\n  # ---------------------------------------------------------------------------\n  # 2b)",
    );
    assert!(sign_stage.contains("needs: [build, sbom, mutation]"));
}
