# find-my-files task runner. Local tools are managed by mise; CI installs the
# same pinned tools through platform-standard setup actions.
# Recipes marked (elevated) need an administrator terminal.
#
# `just` (no args) prints this menu, grouped by area via the [group('…')]
# attributes below. New here? Run `mise install`, `just setup`, then `just check`.

# just defaults to `sh` even on Windows — absent in elevated PowerShell,
# exactly where the admin recipes must run. powershell.exe always exists.
set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

default:
    @just --list --unsorted

# ── Setup ────────────────────────────────────────────────────────────────

# One-time repository setup after `mise install`: install git hooks.
[group('setup')]
setup:
    lefthook install

# Check the dev environment matches mise.toml (run after `mise install`).
# Logic lives in xtask (the doctor subcommand); this is a thin wrapper, and
# --target-dir keeps xtask output under build/ (ADR-0021).
[group('setup')]
[doc('Check the dev environment matches the mise.toml pins (run after mise install)')]
doctor:
    cargo run --locked --manifest-path xtask/Cargo.toml --target-dir build/xtask -- doctor

# ── Daily loop ───────────────────────────────────────────────────────────

# Type-check without codegen — the fast inner loop
[group('daily')]
[working-directory: 'engine']
check: check-contract
    cargo check --locked --workspace --all-targets

# Fast contract-drift tripwire (~sub-second warm): the committed C# bindings
# still match the contract source. Same `--check` assertion as
# drift.rs inside the nextest suite, but it compiles only the dependency-free
# fmf-contract leaf — so `just check` catches a forgotten `just contract-gen`
# without waiting for the whole engine test build (ADR-0018).
[group('daily')]
[doc('Fast contract-drift tripwire — gen-contract --check, sub-second warm')]
[working-directory: 'engine']
check-contract:
    cargo run --locked -q -p fmf-contract --bin gen-contract -- --check

# Build the engine (release binaries)
[group('daily')]
[working-directory: 'engine']
build:
    cargo build --locked --release

# Run every Rust test through nextest, then the doctests cargo-nextest does not run.
[group('daily')]
[doc('Run Rust tests with nextest plus doctests (no elevation)')]
[working-directory: 'engine']
test *args="":
    cargo nextest run --locked --workspace {{args}}
    cargo test --locked --workspace --exclude fmf-ffi --doc

# C# unit tests (no elevation; never rebuilds the Rust engine)
[group('daily')]
test-app:
    dotnet test app/FindMyFiles.Tests --results-directory build/test-results/app -p:SkipRustBuild=true -p:RestoreLockedMode=true

# C# unit tests + coverage gate (line+branch >=57; UI is [ExcludeFromCodeCoverage]).
# Threshold/type/stat live in the test csproj (with ExcludeByFile), so this is just
# -p:CollectCoverage=true and no comma-bearing prop ever reaches the shell. CI runs
# packages.lock.json is enforced locally and in CI; the parameter remains only
# for compatibility with existing CI calls and defaults fail-closed.
[group('daily')]
[doc('C# unit tests + coverage gate (line+branch >=57)')]
test-app-cov locked="true":
    dotnet test app/FindMyFiles.Tests --results-directory build/test-results/app -p:SkipRustBuild=true -p:RestoreLockedMode={{locked}} -p:CollectCoverage=true

# Elevation-gated #[ignore] tests: real-volume MFT/USN and machine-security
# boundaries (elevated). The
# FMF_ADMIN_TESTS gate is set by xtask via Command::env on the child cargo, not
# in the shell — powershell.exe strips the nested quotes from `cargo --config
# 'env.X="1"'` (leaving the bare integer 1, which cargo rejects), so the value
# must never touch a shell. Logic lives in xtask (the test-admin subcommand).
[group('daily')]
[doc('Run elevated real-volume and machine-security tests')]
[working-directory: 'xtask']
test-admin:
    cargo run --locked -- test-admin

# Clippy both Rust workspaces (deny warnings) + repository/workflow linters.
[group('daily')]
lint: lint-engine lint-xtask lint-text lint-actions

[group('daily')]
[doc('Check repository spelling')]
lint-text:
    typos

[group('daily')]
[doc('Validate GitHub Actions syntax and security')]
lint-actions:
    actionlint
    zizmor --offline --strict-collection --persona auditor --min-severity low .

[private]
[working-directory: 'engine']
lint-engine:
    cargo clippy --locked --workspace --all-targets -- -D warnings

[private]
[working-directory: 'xtask']
lint-xtask:
    cargo clippy --locked --all-targets -- -D warnings

# Format Rust (engine + xtask workspaces) and all TOML (repo-wide, taplo.toml).
[group('daily')]
fmt:
    cargo fmt --manifest-path engine/Cargo.toml --all
    cargo fmt --manifest-path xtask/Cargo.toml --all
    taplo fmt

# Verify Rust + TOML formatting. C# style/format/analyzers are enforced by the
# build itself (EnforceCodeStyleInBuild + AnalysisMode=All + warnings-as-errors),
# exercised by `test-app` — so `verify` below also covers C#.
[group('daily')]
[doc('Check Rust + TOML formatting (C# format is enforced by the build)')]
fmt-check: fmt-check-engine fmt-check-xtask fmt-check-toml

[group('daily')]
[doc('Check engine Rust formatting')]
fmt-check-engine:
    cargo fmt --manifest-path engine/Cargo.toml --all -- --check

[group('daily')]
[doc('Check xtask Rust formatting')]
fmt-check-xtask:
    cargo fmt --manifest-path xtask/Cargo.toml --all -- --check

[group('daily')]
[doc('Check repository TOML formatting')]
fmt-check-toml:
    taplo fmt --check

# Everything the pre-push hook checks, in one shot
[group('daily')]
verify: fmt-check lint test test-xtask test-app deny machete

# The dispatched release workflow is the already-linted protected-main workflow;
# its checkout is build input, not workflow code. Re-run every source and
# dependency gate there without requiring the Linux-only actionlint verifier on
# the Windows release runner.
[private]
verify-release-source: fmt-check lint-engine lint-xtask lint-text test test-xtask test-app deny machete

# xtask is intentionally a separate binary-only workspace, so its nextest lane
# is explicit rather than silently omitted from `verify`.
[group('daily')]
[working-directory: 'xtask']
test-xtask:
    cargo nextest run --locked

# Intentional dependency-update ceremony for the main engine workspace.
[group('setup')]
[doc('Resolve engine dependency changes into its committed lockfile')]
[working-directory: 'engine']
engine-lock:
    cargo check

# Intentional dependency-update ceremony for the standalone xtask workspace.
[group('setup')]
[doc('Resolve xtask dependency changes into its committed lockfile')]
[working-directory: 'xtask']
xtask-lock:
    cargo check

# Time the full pre-push gate exactly as the hook runs it — per-job timings come
# from lefthook itself, so no shell timing logic lives in the recipe.
[group('daily')]
[doc('Run the whole pre-push gate via lefthook, with per-job timings')]
verify-timed:
    lefthook run pre-push

# Background cargo watcher for the engine inner loop (bacon): recompiles on save
# and shows only the errors. Defaults to clippy to mirror the lint gate — config
# in engine/bacon.toml. Quit with q/Esc.
[group('daily')]
[doc('Background cargo watcher for the engine (bacon) — recompile on save')]
[working-directory: 'engine']
dev:
    bacon

# Regenerate app/FindMyFiles/Engine/Generated/EngineContract.g.cs from the
# contract single source (ADR-0018). The nextest suite runs the drift check.
[group('daily')]
[doc('Regenerate the C# EngineContract bindings from the contract source (ADR-0018)')]
[working-directory: 'engine']
contract-gen:
    cargo run --locked -p fmf-contract --bin gen-contract

[group('daily')]
[doc('Explicitly recapture the shared wire/JSON golden corpus (intentional contract changes only)')]
[working-directory: 'xtask']
contract-bless:
    cargo run --locked -- contract-bless

# Assemble the distributable bundle in build/dist/FindMyFiles: PUBLISHED app (not a
# bare `dotnet build` — the WinUI component package only wires WinRT.Runtime.dll,
# the WinAppSDK native helpers and the compiled XAML into the *publish* output)
# plus the service executable. The clean/publish/locale-
# prune/copy/self-verify logic + the prune predicate's tests live in xtask.
# skip_rust=true skips the in-build cargo step — for CI, where the engine
# binaries are prebuilt and downloaded into build/engine/release/ before this
# runs. --release: this path runs in CI uncached, and `package` (release builds)
# wants a non-debug deflate.
# working-directory xtask (not --manifest-path from root): cargo discovers
# .cargo/config.toml from the CWD, so target-dir → build/xtask only when run
# from inside xtask/ (ADR-0021).
[group('release')]
[doc('Assemble the distributable bundle into build/dist/FindMyFiles')]
[working-directory: 'xtask']
publish-app skip_rust="false":
    cargo run --locked --release -- publish --skip-rust {{skip_rust}}

# Normalize the three cargo-sbom 0.10 entry-point graphs and derive the .NET
# runtime graph from the final self-contained dist + NuGet restore evidence.
# The raw directory is job-local and must contain exactly the three expected
# files; all validation/serialization logic lives in xtask.
[group('release')]
[doc('Generate deterministic artifact-derived CycloneDX 1.6 SBOMs')]
[working-directory: 'xtask']
sbom version cargo_raw_dir:
    cargo run --locked --release -- sbom "{{version}}" --cargo-raw-dir "{{cargo_raw_dir}}"

[group('release')]
[doc('Verify build/sbom is exactly the canonical final BOM pair for a version')]
[working-directory: 'xtask']
sbom-verify version:
    cargo run --locked --release -- sbom-verify "{{version}}"

# Compile deterministic fake/unavailable-engine seams into a physically separate,
# non-packaged tree. Stable build/dist never contains these launch arguments.
[group('release')]
[working-directory: 'xtask']
publish-ui-test skip_rust="false":
    cargo run --locked --release -- publish-ui-test --skip-rust {{skip_rust}}

# Local/release publish: build the engine first, then publish (rust is already
# built, so the in-build cargo step is skipped).
[group('release')]
[doc('Build the engine, then assemble the distributable bundle')]
publish: build (publish-app "true")

# ── Service (v2: fmf-service + named pipe; ADR-0016/0017) ────────────────

# Console-mode service in the foreground — the dev inner loop (elevated;
# Ctrl+C = flush + graceful stop). Unelevated pipe debugging: add --no-index
[group('service')]
[doc('Run fmf-service in the foreground — the dev inner loop (elevated)')]
[working-directory: 'engine']
service-dev *args="":
    cargo run --locked --release -p fmf-service -- run {{args}}

# Build fmf-service (release)
[group('service')]
[working-directory: 'engine']
service-build:
    cargo build --locked --release -p fmf-service

# Register the on-demand Windows service: captures your SID, hardens the
# data-dir DACLs, copies the stable service binary and configures recovery
# (elevated once).
[group('service')]
service-install: service-build
    build/engine/release/fmf-service.exe install

# Deregister; data stays unless you pass --purge-data (elevated)
[group('service')]
service-uninstall *args="":
    build/engine/release/fmf-service.exe uninstall {{args}}

# Unelevated after install: the per-service DACL grants start only.
[group('service')]
service-start:
    build/engine/release/fmf-service.exe start

# Unelevated after install: the per-service DACL grants stop only.
[group('service')]
service-stop:
    build/engine/release/fmf-service.exe stop

# Refresh the ProgramData stable copy/configuration, then restart (elevated).
# A plain rebuild cannot update the already-installed service image.
[group('service')]
service-restart: service-stop service-install service-start

# SCM state + live pipe handshake (works unelevated)
[group('service')]
service-status:
    build/engine/release/fmf-service.exe status

# C# client × real fmf-service integration (FMF_PIPE_TESTS gate; no elevation)
[group('service')]
test-pipe: service-build
    dotnet test app/FindMyFiles.Tests --settings app/FindMyFiles.Tests/pipe.runsettings --results-directory build/test-results/pipe -p:SkipRustBuild=true -p:RestoreLockedMode=true

# winapp UI-automation release suite (no elevation). Publishes the isolated
# test-seam bundle, then
# hands the published apphost (app/FindMyFiles.exe, NOT the root launcher — that
# spawns-and-exits, so automation must attach to the real app) to ui-tests.ps1,
# which launches it under --engine=unavailable (setup screen) and --fake-engine
# (search) and asserts on the AutomationIds. The script owns process lifecycle;
# this recipe is a thin pwsh wrapper. -IncludeFaults requires a DEBUG bundle.
[group('service')]
[doc('winapp UI-automation release suite (publishes the bundle; no elevation)')]
ui-test: publish (publish-ui-test "true") ui-test-stable-smoke ui-test-published

# Exercise an already-assembled UI-test bundle. CI builds this separately from
# the shippable `build/dist` tree so test-only engine/data-dir switches cannot
# leak into release binaries.
# it reuses the upstream engine artifacts instead of rebuilding Rust.
[group('service')]
[doc('Run UI automation against the existing published bundle')]
ui-test-published:
    pwsh -NoProfile -ExecutionPolicy Bypass -File app/FindMyFiles.Tests/UiAutomation/ui-tests.ps1 -ExePath build/ui-test-bundle/FindMyFiles/app/FindMyFiles.exe -OutDir build/ui-automation

# Launch the exact shipping binary without any compile-time test seams and prove
# its real WinUI tree initializes. Full deterministic interaction coverage uses
# the separate bundle above.
[group('service')]
ui-test-stable-smoke:
    pwsh -NoProfile -ExecutionPolicy Bypass -File app/FindMyFiles.Tests/UiAutomation/ui-tests.ps1 -StableSmoke -ExePath build/dist/FindMyFiles/app/FindMyFiles.exe -OutDir build/ui-automation-stable

# ── Benchmarks & gates (discipline: ADR-0013) ───────────────────────────

# Run the benchmark query set against a real volume (elevated)
[group('bench')]
[working-directory: 'engine']
bench drive="C:" *args="":
    cargo run --locked --release -p fmf-cli -- bench {{drive}} {{args}}

# Enforce ADR-0013's cold/idle measurement precondition with Windows' own
# processor counters. Pure procedural logic lives in xtask, not shell.
[group('bench')]
[working-directory: 'xtask']
perf-preflight:
    cargo run --locked -- perf-preflight

# Real-volume regression gate vs the committed baseline. xtask compiles first,
# then owns preflight + whole-run monitoring + postflight as one transaction.
[group('bench')]
[working-directory: 'xtask']
bench-check drive="C:":
    cargo run --locked -- perf-real-check {{drive}}

# (Re)record the committed real-volume baseline. The candidate carries its
# measurement identity and is atomically promoted only after postflight. Since
# ADR-0048 there is no CI baseline-proposal path: record here on the reference
# machine and land engine/benches/baseline.json through an ordinary PR.
[group('bench')]
[working-directory: 'xtask']
bench-baseline drive="C:":
    cargo run --locked -- perf-real-baseline {{drive}}

# Criterion micro-benchmarks on the synthetic 1M index (no elevation)
[group('bench')]
[working-directory: 'engine']
bench-micro *args="":
    cargo bench --locked -p fmf-core --features testutil {{args}}

# Lives in build/engine/criterion (machine-local; gone on cargo clean).
# Record a complete candidate and atomically promote the whole baseline tree.
[group('bench')]
[working-directory: 'xtask']
bench-micro-baseline:
    cargo run --locked -- perf-micro-baseline

# Compare in a fresh CRITERION_HOME. The gate requires all 28 current reports
# and fails only when the median 95% CI lower bound exceeds +10%.
[group('bench')]
[working-directory: 'xtask']
bench-micro-check:
    cargo run --locked -- perf-micro-check

# The performance gate. Run it before merging fmf-core changes AND, since
# ADR-0048 retired the CI measurement chain, by hand on the reference machine
# before approving a release's `sign` job — it is the release performance gate,
# not a mechanical precondition CI can enforce (DEV-287/DEV-321). Each half
# performs its own compile/preflight/monitor/postflight sequence; just cannot
# dedupe it.
[group('bench')]
perf-gate: bench-check bench-micro-check

# ── Volume tools (elevated) ──────────────────────────────────────────────

# Index a volume, print scan stats, drop into the query REPL
[group('volume')]
[working-directory: 'engine']
index drive="C:":
    cargo run --locked --release -p fmf-cli -- index {{drive}} --stats

# Per-column memory accounting (the B/entry RAM gate figure)
[group('volume')]
[working-directory: 'engine']
stats drive="C:" *args="":
    cargo run --locked --release -p fmf-cli -- stats {{drive}} {{args}}

# Name/size distribution — the input for pool/column layout decisions
[group('volume')]
[working-directory: 'engine']
name-stats drive="C:":
    cargo run --locked --release -p fmf-cli -- stats {{drive}} --name-stats

# $MFT read-throughput probe per I/O strategy (verdicts: ADR-0011)
[group('volume')]
[working-directory: 'engine']
io-probe drive="C:" mode="buffered" *args="":
    cargo run --locked --release -p fmf-cli -- io-probe {{drive}} --mode {{mode}} {{args}}

# Machine code is identical to release — only debuginfo is upgraded.
# Profile fmf-cli under samply (ETW; elevated), e.g. `just profile bench C:`
[group('volume')]
[working-directory: 'engine']
profile *args="bench C:":
    cargo build --locked --profile profiling -p fmf-cli
    samply record -- ../build/engine/profiling/fmf-cli {{args}}

# ── Fuzz (Linux/nightly; CI fuzz.yml runs this on every wire-codec change) ─

# libFuzzer over the pipe wire codec (fmf-proto/fmf-contract — the privilege
# boundary). Needs nightly + cargo-fuzz on Linux/WSL (flaky on Windows).
# Run from engine/ so cargo-fuzz finds ./fuzz (no --fuzz-dir = version-proof).
# e.g. `just fuzz message_decode 120`
[group('fuzz')]
[doc('libFuzzer over the pipe wire codec (nightly + cargo-fuzz; Linux/WSL)')]
[working-directory: 'engine']
fuzz target="frame_decode" secs="60": fuzz-lock-check
    cargo +nightly fuzz run {{target}} -- -max_total_time={{secs}}

# Compile all fuzz targets without running them (fast harness sanity check).
[group('fuzz')]
[working-directory: 'engine']
fuzz-build: fuzz-lock-check
    cargo +nightly fuzz build

# cargo-fuzz 0.13.2 does not forward Cargo's --locked flag. Fail before it
# starts unless the standalone fuzz workspace's committed lockfile is exact.
[private]
[working-directory: 'engine']
fuzz-lock-check:
    cargo metadata --locked --manifest-path fuzz/Cargo.toml --format-version 1 --no-deps

# Intentional dependency-update ceremony for the standalone fuzz workspace.
[group('fuzz')]
[doc('Regenerate the standalone fuzz workspace lockfile intentionally')]
[working-directory: 'engine']
fuzz-lock:
    cargo generate-lockfile --manifest-path fuzz/Cargo.toml

# ── Hygiene ──────────────────────────────────────────────────────────────

# Sweep leftover TestDir fixtures (build/engine/test-tmp). Their Drop-time
# removal is best-effort, so killed test runs can leave directories behind;
# cargo clean also removes them, this is the cheaper broom.
[group('hygiene')]
[doc('Sweep leftover TestDir fixtures (build/engine/test-tmp)')]
[working-directory: 'xtask']
clean-temp:
    cargo run --locked -- clean-temp

# ── Release ──────────────────────────────────────────────────────────────

# NOTE: there is intentionally no `just release` recipe. Versioning, the
# CHANGELOG and the vX.Y.Z tag are owned by release-please (Conventional Commits
# → an auto-maintained Release PR; merging it cuts the tag, and release-please.yml
# then dispatches release.yml from protected main with that exact tag, commit,
# and draft release ID). Humans never hand-pick or hand-edit a version.
# See ADR-0035 and ADR-0048.

# Print the canonical channel-aware build version (the FMF_BUILD_VERSION format).
# dev/nightly/stable; nightly needs --date. Used by nightly.yml + release.yml.
# Usage:  just version --channel nightly --date 20260629
[group('release')]
[doc('Print the channel-aware build version string')]
[working-directory: 'xtask']
version *args="":
    cargo run --locked -- version {{args}}

# Zip + checksum the assembled bundle (run AFTER publish + signing). The payload's
# BUILDINFO.txt is the identity source: a vX.Y.Z tag must match it exactly; without
# a tag, dev/nightly use that identity verbatim. Both land + SHA256SUMS.txt under
# build/package/ — the assets release.yml attaches. --release: deflate wants a
# non-debug build. Usage:  just package v0.2.0   (or)   just package
[group('release')]
[doc('Zip + checksum the assembled bundle using its BUILDINFO identity')]
[working-directory: 'xtask']
package tag="":
    cargo run --locked --release -- package {{tag}}

# Verify a release tag (vX.Y.Z) matches the committed [workspace.package] version
# in engine/Cargo.toml — the manual-dispatch guard release.yml runs before
# signing/packaging so a drifted tag can't ship mislabeled artifacts. Usage:
# just check-version v0.2.0. Logic lives in xtask.
[group('release')]
[doc('Verify a release tag matches the committed workspace version')]
[working-directory: 'xtask']
check-version tag:
    cargo run --locked --release -- check-version {{tag}}

# Stage the bundle's first-party PEs into sign-stage/ (unique names) for the
# release signing step, and copy the signed copies back from signed/ afterwards.
# The map of what-we-sign lives in xtask (publish::FIRST_PARTY_PES), not in the
# workflow; release.yml calls these around the eSigner Action.
[group('release')]
[doc('Stage first-party PEs for release signing')]
[working-directory: 'xtask']
sign-stage:
    cargo run --locked --release -- sign-stage

[group('release')]
[doc('Copy signed PEs back into the bundle after signing')]
[working-directory: 'xtask']
sign-collect:
    cargo run --locked --release -- sign-collect

[group('release')]
[doc('Seal the exact unsigned or signed release bundle into a deterministic manifest')]
[working-directory: 'xtask']
bundle-seal state:
    cargo run --locked --release -- bundle-seal {{state}}

[group('release')]
[doc('Verify the exact unsigned or signed bundle against its canonical manifest')]
[working-directory: 'xtask']
bundle-verify state:
    cargo run --locked --release -- bundle-verify {{state}}

[group('release')]
[doc('Verify signing changed only canonical first-party PE certificate regions')]
[working-directory: 'xtask']
bundle-verify-signed-transition:
    cargo run --locked --release -- bundle-verify-signed-transition

# ── Docs ─────────────────────────────────────────────────────────────────

# Validate the canonical prose and Rust doc comments. Only mdBook is published;
# implementation API pages are intentionally not a product surface.
[group('docs')]
[doc('Validate canonical docs and internal Rust doc comments')]
doc:
    mdbook build docs
    cargo doc --locked --config "build.rustdocflags=['-D','warnings']" --no-deps --workspace --document-private-items --manifest-path engine/Cargo.toml --target-dir build/engine

# Live-preview the design docs at http://localhost:3000
[group('docs')]
doc-serve:
    mdbook serve docs --open

# Stage landing + canonical book into build/site. Run `just doc` first.
[group('docs')]
[doc('Stage landing + canonical book into build/site')]
[working-directory: 'xtask']
docs-assemble:
    cargo run --locked -- docs-assemble

# ── Quality gates (also enforced in CI) ──────────────────────────────────

# Rust line coverage (cargo-llvm-cov). CI gates with --fail-under-lines.
[group('quality')]
[working-directory: 'engine']
cov:
    # LLVM instrumentation invalidates the 5 ms wall-clock pipe budget. That
    # test remains mandatory in ordinary nextest and the dedicated perf gates.
    cargo llvm-cov nextest --locked --workspace --profile ci --fail-under-lines 76 --summary-only -E 'not test(page_roundtrip_stays_inside_the_latency_budget)'

# License / ban / source policy (cargo-deny). Advisories live in cargo-audit.
[group('quality')]
[working-directory: 'engine']
deny:
    cargo deny check bans licenses sources

# Unused dependencies (cargo-machete).
[group('quality')]
machete:
    cargo machete engine

# Mutation testing (Rust, ADR-0022). xtask owns the fixed cargo-mutants 27.1.0
# invocation, requires its unmutated nextest baseline, canonicalizes exact
# survivors, rejects timeouts/malformed reports, and compares the reviewed
# rationale-bearing identity baseline. It intentionally accepts no CLI options.
[group('quality')]
[doc('Strict Rust mutation gate (exact survivors; ADR-0022)')]
[working-directory: 'xtask']
mutants:
    cargo run --locked --release -- mutation-rust

# Mutation testing (C#, Stryker.NET 4.16.0 — ADR-0022). xtask first runs the
# ordinary locked unit-test baseline, makes Stryker fail on an initial-test
# failure, pins the exact reviewed mutate-file inventory, parses its strict JSON
# report, rejects inconclusive/out-of-scope results, and compares exact
# survivors. It intentionally accepts no CLI options.
[group('quality')]
[doc('Strict C# mutation gate (exact survivors; ADR-0022)')]
[working-directory: 'xtask']
stryker:
    cargo run --locked --release -- mutation-csharp

[group('quality')]
[doc('Run both exact-identity mutation gates (slow)')]
mutation: mutants stryker
