//! Structural tripwires for the trusted exact-identity mutation gates
//! (ADR-0022).
//!
//! Runtime parsers prove report semantics. These tests pin the surrounding
//! trust split: default-branch caller -> same-commit reusable controller ->
//! separate immutable target data checkout -> fresh evidence verifier.

const MUTATION_LOCAL_SOURCE: &str = include_str!("../src/mutation.rs");
const MUTATION_CI_SOURCE: &str = include_str!("../src/mutation_ci.rs");
const MUTANTS_WORKFLOW: &str = include_str!("../../.github/workflows/mutants.yml");
const CONTROLLER_WORKFLOW: &str = include_str!("../../.github/workflows/mutation-controller.yml");
const RELEASE_WORKFLOW: &str = include_str!("../../.github/workflows/release.yml");
const JUSTFILE: &str = include_str!("../../justfile");
const MISE: &str = include_str!("../../mise.toml");
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
    assert!(CONTROLLER_WORKFLOW.contains("dotnet-version: 10.0.401"));
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

/// `app/FindMyFiles.Tests/stryker-config.json` is the *only* place this
/// repository decides how C# mutants are generated and judged — every key, with
/// its reviewed value.
///
/// It was not. The CI shard runner built a rival configuration object in code,
/// and the two said different things: CI mutated at `Complete` level with
/// coverage analysis off, `vstest` and a `Release` build, while this file named
/// none of that and `just stryker` therefore ran Stryker's `Standard` default
/// under `Debug` with `perTest` coverage. Both then compared their results to
/// the same 103-entry reviewed baseline, so a mutant killed in one lane and
/// alive in the other was a structural property of the gate rather than a
/// finding about the code — and no test could see it, because each lane only
/// ever read its own copy.
///
/// `every_csharp_lane_runs_the_one_reviewed_configuration` (in `mutation_ci.rs`)
/// proves the two lanes hand Stryker the same settings. That equality is hollow
/// on its own: dropping a key here keeps both lanes equal and silently returns
/// both to a Stryker default. This pins the values themselves, so a deletion
/// fails instead of passing.
///
/// Load-bearing, key by key:
/// * `mutation-level: Complete` — Stryker's default is `Standard`, which
///   generates strictly fewer mutants. Nothing else in the tree asks for the
///   thorough set.
/// * `coverage-analysis: off` and `disable-mix-mutants: true` — every mutant is
///   run against the full suite, one at a time. Coverage-driven selection and
///   mixed mutants are how a survivor becomes unattributable to a single
///   identity, and identity is what the baseline is written in.
/// * `test-runner: vstest`, `configuration: Release`, `target-framework` — the
///   gate must mutate the program the product ships, not a Debug build under a
///   different runner.
/// * `break-on-initial-test-failure: true` — an unmutated suite that already
///   fails makes every subsequent verdict meaningless.
/// * `additional-timeout: 30000` — the reviewed scope includes pipe integration
///   tests; 3s (the default) turns otherwise-killed mutants into timeouts, and
///   `validate_stryker_exit` never accepts a timeout.
/// * `concurrency: 2` — Stryker recommends at most two sessions on an ordinary
///   runner, and four made otherwise-killed mutants time out under hosted
///   Windows contention (PR #187).
/// * `reporters` must include `json` — the gate parses `mutation-report.json`;
///   a score-only reporter leaves it nothing to read.
/// * `thresholds.break: 0` — load-bearing where `high`/`low` are report
///   colouring. `validate_stryker_exit` requires exit 0; any break threshold
///   above the achieved score makes Stryker exit 1, failing the gate on a score
///   rather than on the survivor identities it actually judges. The reviewed
///   80/60 are Stryker's own defaults, and with 103 accepted equivalents the
///   100/100 CI used to inject named a state that can never occur.
///
/// `mutate` is deliberately not pinned to a literal here: it is proven against
/// `mutation-baseline.json` through the gate's own resolver by
/// `mutation::tests::csharp_mutate_scope_and_baseline_name_the_same_files`, and
/// a second list would only give the two lists something to drift from.
#[test]
fn the_reviewed_stryker_configuration_is_the_one_both_lanes_run() {
    let config: serde_json::Value =
        serde_json::from_str(STRYKER_CONFIG).expect("stryker-config.json must be valid JSON");
    let root = config
        .get("stryker-config")
        .and_then(serde_json::Value::as_object)
        .expect("stryker-config root");

    let expected = [
        ("project", serde_json::json!("FindMyFiles.csproj")),
        (
            "test-projects",
            serde_json::json!(["FindMyFiles.Tests.csproj"]),
        ),
        ("concurrency", serde_json::json!(2)),
        ("additional-timeout", serde_json::json!(30_000)),
        ("mutation-level", serde_json::json!("Complete")),
        ("coverage-analysis", serde_json::json!("off")),
        ("disable-mix-mutants", serde_json::json!(true)),
        ("test-runner", serde_json::json!("vstest")),
        ("configuration", serde_json::json!("Release")),
        (
            "target-framework",
            serde_json::json!("net10.0-windows10.0.26100.0"),
        ),
        ("break-on-initial-test-failure", serde_json::json!(true)),
        (
            "thresholds",
            serde_json::json!({"high": 80, "low": 60, "break": 0}),
        ),
        ("report-file-name", serde_json::json!("mutation-report")),
        ("reporters", serde_json::json!(["progress", "json"])),
    ];
    for (key, value) in &expected {
        assert_eq!(
            root.get(*key),
            Some(value),
            "reviewed Stryker configuration key `{key}`"
        );
    }

    // Exactly these keys and `mutate`: an unreviewed key is a setting nobody
    // decided, and it would reach CI too, because CI now reads this file.
    let mut keys: Vec<&str> = root.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut allowed: Vec<&str> = expected.iter().map(|(key, _)| *key).collect();
    allowed.push("mutate");
    allowed.sort_unstable();
    assert_eq!(keys, allowed);
    assert!(
        root.get("mutate")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|patterns| !patterns.is_empty()),
        "the reviewed mutate inventory must not be empty"
    );
}

/// Both Stryker lanes read the reviewed configuration instead of restating it,
/// and the controller supplies the SDK selector the target may not.
///
/// The unit tests next to the code prove the values agree. This pins the
/// surrounding structure they cannot see: that the settings live in exactly one
/// file, that every Stryker spawn is built by the one shared argument builder,
/// and that both halves of the `global.json` policy substitution stay — the
/// target forbidden from supplying one, the controller generating and sealing
/// its own. Only the first half of that had ever been implemented, which is why
/// `dotnet --version` answered with whatever SDK the runner image shipped last.
#[test]
fn the_csharp_lanes_take_stryker_policy_and_the_sdk_only_from_the_controller() {
    let local = executable_code(MUTATION_LOCAL_SOURCE);
    let ci = executable_code(MUTATION_CI_SOURCE);

    // No lane may spell a setting the reviewed configuration owns — neither as
    // a rival JSON literal nor on the command line, where Stryker silently
    // prefers it over the file. `thresholds` is spelled with its colon because
    // the bare word is also a key of the Stryker *report* schema, which
    // `parse_csharp_report` legitimately names; only the object-literal form is
    // a rival configuration.
    for (lane, source) in [("mutation.rs", &local), ("mutation_ci.rs", &ci)] {
        for forbidden in [
            "--break-on-initial-test-failure",
            "\"mutation-level\"",
            "\"coverage-analysis\"",
            "\"disable-mix-mutants\"",
            "\"test-runner\"",
            "\"reporters\"",
            "\"thresholds\": ",
        ] {
            assert!(
                !source.contains(forbidden),
                "{lane} spells `{forbidden}` instead of deferring to app/FindMyFiles.Tests/stryker-config.json"
            );
        }
    }

    // One argument builder, used by both lanes and defined once.
    assert_eq!(local.matches("pub fn stryker_args(").count(), 1);
    assert_eq!(local.matches("stryker_args(&output_arg, None)").count(), 1);
    assert_eq!(
        ci.matches("mutation::stryker_args(&output_arg, Some(config_name))")
            .count(),
        1
    );
    // ...and one reviewed file, resolved for the local repository and for the
    // protected controller checkout by the same derivation.
    assert!(local.contains("paths::csharp_stryker_config()"));
    assert!(ci.contains("paths::csharp_stryker_config_in(&controller_root())"));

    // The target supplies no SDK selector...
    assert!(ci.contains("file.eq_ignore_ascii_case(\"global.json\")"));
    // ...and the controller supplies one in its place, sealed like every other
    // policy input so a shard cannot run under a different SDK than the evidence
    // claims.
    for supplied in [
        "let global_json = trusted_global_json()?;",
        "write_bytes(&work.join(\"global.json\"), &global_json)?;",
        "policy_seal(\"generated:global.json\", &global_json)",
        "\"rollForward\": \"disable\"",
    ] {
        assert!(
            ci.contains(supplied),
            "the controller no longer supplies `{supplied}`"
        );
    }
}

/// The .NET SDK pin is one version, however many places spell it.
///
/// Three do: `DOTNET_SDK_VERSION` in `mutation_ci.rs` (what the gate demands and
/// what the generated `global.json` selects), `mise.toml` (what a developer
/// machine installs), and `actions/setup-dotnet` in the controller workflow
/// (what the runner downloads). The gate compares `dotnet --version` to the
/// first for exact equality, so a bulk tool-pin bump (issue #175) that moves the
/// other two and forgets it fails every C# shard — and fails it a week later, in
/// a scheduled run nobody is watching, having blocked `release.yml`'s
/// `sign-stage` in the meantime. `validate_mise_dotnet_pin` catches the
/// `mise.toml` half at run time; the workflow half only exists here.
#[test]
fn the_dotnet_sdk_pin_is_one_version_in_every_spelling() {
    let sdk = quoted_const(MUTATION_CI_SOURCE, "DOTNET_SDK_VERSION");
    assert!(
        MISE.contains(&format!("dotnet = \"{sdk}\"")),
        "mise.toml does not pin dotnet {sdk}"
    );
    let installed: Vec<&str> = CONTROLLER_WORKFLOW
        .lines()
        .filter_map(|line| line.trim().strip_prefix("dotnet-version:"))
        .map(str::trim)
        .collect();
    assert_eq!(
        installed,
        vec![sdk.as_str()],
        "the mutation controller must install exactly the pinned SDK, once"
    );
}

/// The literal behind a `const NAME: &str = "..."` declaration.
///
/// `mutation_workflow_guards` is an integration test against a binary crate, so
/// it cannot import the constant; reading the source is how every tripwire here
/// reaches into the gate.
fn quoted_const(source: &str, name: &str) -> String {
    let (_, tail) = source
        .split_once(&format!("const {name}: &str = \""))
        .unwrap_or_else(|| panic!("missing `const {name}`"));
    let (value, _) = tail
        .split_once('"')
        .unwrap_or_else(|| panic!("unterminated `const {name}`"));
    value.to_owned()
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

/// Production code only: the `#[cfg(test)]` module below it asserts things
/// *about* forbidden options and therefore has to name them, and this
/// repository explains a rule in the comment above the code that implements it.
/// Both would trip a tripwire that searched the whole file.
fn executable_code(source: &str) -> String {
    let (production, _) = source
        .split_once("\n#[cfg(test)]\n")
        .unwrap_or((source, ""));
    production
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A target checkout supplies no mutation policy, and the controller supplies
/// its own in its place.
///
/// Only the first half of that was ever implemented for Rust. `mutants.toml`
/// and `mutation-baseline.json` were correctly excluded from the copied target
/// tree, and then the run passed `--no-config` instead of the controller's
/// copy, so sixteen shards mutated the whole workspace against no baseline at
/// all: five consecutive weekly audits failed with ~121 survivors per shard,
/// nearly all of them in the one file `mutants.toml` documents as unmutatable
/// under this gate, while `just mutants` was green on the same commits. Because
/// `release.yml`'s `sign-stage` needs `mutation`, that also blocked signing.
///
/// `every_rust_lane_runs_the_one_reviewed_scope` (in `mutation_ci.rs`) proves
/// the three argument vectors agree. This pins the surrounding structure that
/// unit test cannot see: that both halves of the policy substitution stay, and
/// that every cargo-mutants spawn in the tree is built by the shared builders
/// rather than spelled out again at the call site.
#[test]
fn the_rust_lanes_take_scope_and_baseline_only_from_the_controller() {
    let local = executable_code(MUTATION_LOCAL_SOURCE);
    let ci = executable_code(MUTATION_CI_SOURCE);

    // Neither lane may opt out of the reviewed scope, and neither may restate
    // an option `engine/mutants.toml` already answers (cargo-mutants prefers
    // the command line without complaining).
    for (lane, source) in [("mutation.rs", &local), ("mutation_ci.rs", &ci)] {
        for forbidden in [
            "--no-config",
            "\"--test-tool\"",
            "\"--timeout-multiplier\"",
            "\"--skip-calls-defaults\"",
        ] {
            assert!(
                !source.contains(forbidden),
                "{lane} passes `{forbidden}` instead of deferring to engine/mutants.toml"
            );
        }
    }

    // Every cargo-mutants process in the repository is spawned from the shared
    // builders, so a new lane cannot quietly assemble its own scope. (The
    // `mutants --version` pins are argument slices, not `.arg("mutants")`.)
    assert_eq!(local.matches(".arg(\"mutants\")").count(), 1);
    assert_eq!(ci.matches(".arg(\"mutants\")").count(), 2);
    assert_eq!(local.matches(".args(rust_run_args(").count(), 1);
    assert_eq!(ci.matches("mutation::rust_run_args(").count(), 1);
    // The nextest policy is one constant in one place. A second copy is a second
    // place for the lanes to drift, which is how `just mutants` ended up running
    // the repository's ordinary profile — 60-second test kill and all — while CI
    // ran a profile with no test timeout at all.
    assert_eq!(local.matches("pub const NEXTEST_POLICY").count(), 1);
    assert!(!ci.contains("const NEXTEST_POLICY"));
    assert!(!ci.contains("const RUST_MUTATION_NEXTEST_ARGS"));
    assert_eq!(ci.matches("mutation::rust_scope_args(config)").count(), 1);

    // The target contributes no policy...
    for excluded in ["engine/mutants.toml", "engine/mutation-baseline.json"] {
        assert_eq!(
            ci.matches(&format!("path != \"{excluded}\"")).count(),
            2,
            "both language copy filters must keep `{excluded}` out of the work tree"
        );
    }
    // ...and the controller supplies both files in its place, for the Rust lane
    // exactly as for the C# lane.
    for supplied in [
        "paths::rust_mutants_config_in(&controller_root())",
        "mutation::read_rust_reviewed_policy(&controller_root())",
        "mutation::read_csharp_reviewed_policy(&controller_root())",
    ] {
        assert!(
            ci.contains(supplied),
            "the controller no longer supplies `{supplied}`"
        );
    }

    // Both the shard that produces evidence and the fresh verifier that grades
    // it project the reviewed survivors themselves, in both languages. A run
    // that decides which survivors are acceptable, or a verifier that takes the
    // shard's word for it, is the same hole in a different place — and with
    // today's empty `accepted_equivalents` no runtime assertion can tell the
    // difference, because every projection is empty.
    for language in ["rust", "csharp"] {
        assert_eq!(
            ci.matches(&format!("reviewed_{language}_survivors(&reviewed_policy"))
                .count(),
            2,
            "the {language} producer and verifier must each project the reviewed policy"
        );
    }
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
