# find-my-files task runner. Requires: mise (rust/dotnet), see mise.toml.
# Recipes marked (elevated) need an administrator terminal.
#
# `just` (no args) prints this menu, grouped by area via the [group('…')]
# attributes below. New here? Run `just setup`, then `just check`.

# just defaults to `sh` even on Windows — absent in elevated PowerShell,
# exactly where the admin recipes must run. powershell.exe always exists.
set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

default:
    @just --list --unsorted

# ── Setup ────────────────────────────────────────────────────────────────

# One-time setup: install pinned toolchain + git hooks
[group('setup')]
[working-directory: 'engine']
setup:
    mise install
    lefthook install

# Check the dev environment matches mise.toml (run right after `just setup`).
# Logic lives in xtask (the doctor subcommand); this is a thin wrapper, and
# --target-dir keeps xtask output under build/ (ADR-0021).
[group('setup')]
[doc('Check the dev environment matches the mise.toml pins (run after just setup)')]
doctor:
    cargo run --manifest-path xtask/Cargo.toml --target-dir build/xtask -- doctor

# ── Daily loop ───────────────────────────────────────────────────────────

# Type-check without codegen — the fast inner loop
[group('daily')]
[working-directory: 'engine']
check: check-contract
    cargo check --workspace --all-targets

# Fast contract-drift tripwire (~sub-second warm): the committed C# bindings +
# docs/contract.md still match the contract source. Same `--check` assertion as
# drift.rs inside `cargo test`, but it compiles only the dependency-free
# fmf-contract leaf — so `just check` catches a forgotten `just contract-gen`
# without waiting for the whole engine test build (ADR-0018).
[group('daily')]
[doc('Fast contract-drift tripwire — gen-contract --check, sub-second warm')]
[working-directory: 'engine']
check-contract:
    cargo run -q -p fmf-contract --bin gen-contract -- --check

# Build the engine (release binaries)
[group('daily')]
[working-directory: 'engine']
build:
    cargo build --release

# Run the engine unit tests (cargo, unelevated)
[group('daily')]
[working-directory: 'engine']
test:
    cargo test --workspace

# C# unit tests (no elevation; never rebuilds the Rust engine)
[group('daily')]
test-app:
    dotnet test app/FindMyFiles.Tests -p:SkipRustBuild=true

# C# unit tests + coverage gate (line+branch >=55; UI is [ExcludeFromCodeCoverage]).
# Threshold/type/stat live in the test csproj (with ExcludeByFile), so this is just
# -p:CollectCoverage=true and no comma-bearing prop ever reaches the shell. CI runs
# `just test-app-cov true` (locked enforces packages.lock.json); locally the bare
# recipe reproduces the identical gate.
[group('daily')]
[doc('C# unit tests + coverage gate (line+branch >=55)')]
test-app-cov locked="false":
    dotnet test app/FindMyFiles.Tests -p:SkipRustBuild=true -p:RestoreLockedMode={{locked}} -p:CollectCoverage=true

# Elevation-gated #[ignore] tests: real-volume MFT/USN (elevated).
# Sets the gate env var the PowerShell way on ONE line (just runs each recipe line
# in its own shell, so the assignment must share the line with cargo). `cargo
# --config 'env.X="1"'` is NOT used: the recipe's powershell.exe strips the nested
# quotes, leaving the bare integer 1, which cargo rejects ("expected a string").
[group('daily')]
[doc('Run the elevated, ignore-gated real-volume MFT/USN tests')]
[working-directory: 'engine']
test-admin:
    $env:FMF_ADMIN_TESTS = '1'; cargo test --workspace -- --ignored

# Clippy (deny warnings) + typos
[group('daily')]
[working-directory: 'engine']
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    typos

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
fmt-check:
    cargo fmt --manifest-path engine/Cargo.toml --all -- --check
    cargo fmt --manifest-path xtask/Cargo.toml --all -- --check
    taplo fmt --check

# Everything the pre-push hook checks, in one shot
[group('daily')]
verify: fmt-check lint test test-app

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
# contract single source (ADR-0018). cargo test runs the drift check.
[group('daily')]
[doc('Regenerate the C# EngineContract bindings from the contract source (ADR-0018)')]
[working-directory: 'engine']
contract-gen:
    cargo run -p fmf-contract --bin gen-contract

# Assemble the distributable bundle in build/dist/FindMyFiles: PUBLISHED app (not a
# bare `dotnet build` — the WinUI component package only wires WinRT.Runtime.dll,
# the WinAppSDK native helpers and the compiled XAML into the *publish* output)
# plus the engine binaries (fmf-service.exe / fmf.exe). The clean/publish/locale-
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
    cargo run --release -- publish --skip-rust {{skip_rust}}

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
    cargo run --release -p fmf-service -- run {{args}}

# Build fmf-service (release)
[group('service')]
[working-directory: 'engine']
service-build:
    cargo build --release -p fmf-service

# Register the Windows service: captures your SID, hardens the data-dir
# DACLs, delayed auto start + crash recovery (elevated)
[group('service')]
service-install: service-build
    build/engine/release/fmf-service.exe install

# Deregister; data stays unless you pass --purge-data (elevated)
[group('service')]
service-uninstall *args="":
    build/engine/release/fmf-service.exe uninstall {{args}}

# (elevated)
[group('service')]
service-start:
    build/engine/release/fmf-service.exe start

# (elevated)
[group('service')]
service-stop:
    build/engine/release/fmf-service.exe stop

# Rebuild + restart the installed service (elevated)
[group('service')]
service-restart: service-stop service-build service-start

# SCM state + live pipe handshake (works unelevated)
[group('service')]
service-status:
    build/engine/release/fmf-service.exe status

# C# client × real fmf-service integration (FMF_PIPE_TESTS gate; no elevation)
[group('service')]
test-pipe: service-build
    dotnet test app/FindMyFiles.Tests --settings app/FindMyFiles.Tests/pipe.runsettings -p:SkipRustBuild=true

# winapp UI-automation smoke suite (no elevation). Publishes the bundle, then
# hands the published FindMyFiles.exe to ui-tests.ps1, which launches it under
# --engine=empty (setup screen) and --fake-engine (search) and asserts on the
# AutomationIds. The script owns process lifecycle; this recipe is a thin
# pwsh wrapper. -IncludeFaults requires a DEBUG bundle, so it is off here.
[group('service')]
[doc('winapp UI-automation smoke suite (publishes the bundle; no elevation)')]
ui-test: publish
    pwsh -NoProfile -ExecutionPolicy Bypass -File app/FindMyFiles.Tests/UiAutomation/ui-tests.ps1 -ExePath build/dist/FindMyFiles/FindMyFiles.exe

# ── Benchmarks & gates (discipline: ADR-0013, engine/benches/README.md) ──

# Run the benchmark query set against a real volume (elevated)
[group('bench')]
[working-directory: 'engine']
bench drive="C:" *args="":
    cargo run --release -p fmf-cli -- bench {{drive}} {{args}}

# Real-volume regression gate vs the committed baseline (elevated, cool machine)
[group('bench')]
[working-directory: 'engine']
bench-check drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}} --baseline benches/baseline.json

# Note the machine and entry count in benches/README.md when regenerating.
# (Re)record the committed real-volume baseline (elevated, cool machine)
[group('bench')]
[working-directory: 'engine']
bench-baseline drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}} --json benches/baseline.json

# Criterion micro-benchmarks on the synthetic 1M index (no elevation)
[group('bench')]
[working-directory: 'engine']
bench-micro *args="":
    cargo bench -p fmf-core {{args}}

# Lives in build/engine/criterion (machine-local; gone on cargo clean).
# Record the local criterion baseline — start of every optimization session
[group('bench')]
[working-directory: 'engine']
bench-micro-baseline:
    cargo bench -p fmf-core --bench search -- --save-baseline committed

# Compare against the local baseline; fail on >10% median regressions
[group('bench')]
[working-directory: 'engine']
bench-micro-check:
    cargo bench -p fmf-core --bench search -- --baseline committed
    cargo run --release -p fmf-cli -- criterion-gate --dir ../build/engine/criterion

# Full performance gate before merging fmf-core changes (elevated, cool machine)
[group('bench')]
perf-gate: bench-check bench-micro-check

# ── Volume tools (elevated) ──────────────────────────────────────────────

# Index a volume, print scan stats, drop into the query REPL
[group('volume')]
[working-directory: 'engine']
index drive="C:":
    cargo run --release -p fmf-cli -- index {{drive}} --stats

# Per-column memory accounting (the B/entry RAM gate figure)
[group('volume')]
[working-directory: 'engine']
stats drive="C:" *args="":
    cargo run --release -p fmf-cli -- stats {{drive}} {{args}}

# Name/size distribution — the input for pool/column layout decisions
[group('volume')]
[working-directory: 'engine']
name-stats drive="C:":
    cargo run --release -p fmf-cli -- stats {{drive}} --name-stats

# $MFT read-throughput probe per I/O strategy (verdicts: ADR-0011)
[group('volume')]
[working-directory: 'engine']
io-probe drive="C:" mode="buffered" *args="":
    cargo run --release -p fmf-cli -- io-probe {{drive}} --mode {{mode}} {{args}}

# Machine code is identical to release — only debuginfo is upgraded.
# Profile fmf-cli under samply (ETW; elevated), e.g. `just profile bench C:`
[group('volume')]
[working-directory: 'engine']
profile *args="bench C:":
    cargo build --profile profiling -p fmf-cli
    samply record -- ../build/engine/profiling/fmf-cli {{args}}

# ── Fuzz (Linux/nightly; CI fuzz.yml runs this on every wire-codec change) ─

# libFuzzer over the pipe wire codec (fmf-proto/fmf-contract — the privilege
# boundary). Needs nightly + cargo-fuzz on Linux/WSL (flaky on Windows).
# Run from engine/ so cargo-fuzz finds ./fuzz (no --fuzz-dir = version-proof).
# e.g. `just fuzz message_decode 120`
[group('fuzz')]
[doc('libFuzzer over the pipe wire codec (nightly + cargo-fuzz; Linux/WSL)')]
[working-directory: 'engine']
fuzz target="frame_decode" secs="60":
    cargo +nightly fuzz run {{target}} -- -max_total_time={{secs}}

# Compile all fuzz targets without running them (fast harness sanity check).
[group('fuzz')]
[working-directory: 'engine']
fuzz-build:
    cargo +nightly fuzz build

# ── Hygiene ──────────────────────────────────────────────────────────────

# Sweep leftover TestDir fixtures (build/engine/test-tmp). Their Drop-time
# removal is best-effort, so killed test runs can leave directories behind;
# cargo clean also removes them, this is the cheaper broom.
[group('hygiene')]
[doc('Sweep leftover TestDir fixtures (build/engine/test-tmp)')]
[working-directory: 'xtask']
clean-temp:
    cargo run -- clean-temp

# ── Release ──────────────────────────────────────────────────────────────

# Cut a release: bump the version (Rust workspace + C# app in lockstep),
# commit, and create a signed vX.Y.Z tag. Pushing the tag fires release.yml
# (GITHUB_TOKEN only — no stored secret). Logic + guards (semver, existing-tag,
# no-op) + tests live in xtask; `--dry-run` shows the diff without committing.
# Usage:  just release 0.2.0   then   git push; git push origin v0.2.0
[group('release')]
[doc('Cut a release: bump version, commit, create a signed tag')]
[working-directory: 'xtask']
release version *args="":
    cargo run -- release {{version}} {{args}}

# Zip + checksum the assembled bundle for a release tag (run AFTER publish +
# signing). Outputs find-my-files-v<version>-win-x64.zip + SHA256SUMS.txt under
# build/package/ — the assets release.yml attaches. --release: deflate wants a
# non-debug build. Usage:  just package v0.2.0
[group('release')]
[doc('Zip + checksum the assembled bundle for a release tag')]
[working-directory: 'xtask']
package tag:
    cargo run --release -- package {{tag}}

# ── Docs ─────────────────────────────────────────────────────────────────

# Build every published doc artifact: mdBook design docs (build/docs-book) +
# rustdoc (build/engine/doc) + the C# API reference (build/docs-csharp/_site).
# Same outputs the pages.yml workflow publishes to GitHub Pages.
# --document-private-items: the crates are internal (no external API surface),
# so the docs are for maintainers — private items are the interesting part, and
# documenting them also resolves intra-doc links to non-pub helpers.
[group('docs')]
[doc('Build all published docs: mdBook + rustdoc + C# API reference')]
doc: doc-csharp
    mdbook build docs
    cargo doc --no-deps --workspace --document-private-items --manifest-path engine/Cargo.toml --target-dir build/engine

# Build the C# API reference (build/docs-csharp/_site) from the BUILT
# FindMyFiles.dll + its XML doc. DefaultDocumentation reads the assembly via IL,
# so it works with the current .NET 10 SDK (DocFX's Roslyn path extracts zero
# types — docfx#11046 / #40), and emits Markdown; xtask renders it to HTML with
# mdBook (same renderer as the design docs). The plain `dotnet build` (no
# SkipRustBuild) lets the csproj build fmf_engine.dll itself, so this recipe is
# self-sufficient. DefaultDocumentation is a pinned dotnet local tool.
[group('docs')]
[doc('Build the C# API reference (DefaultDocumentation to mdBook)')]
doc-csharp:
    dotnet build app/FindMyFiles -c Release
    dotnet tool restore
    cargo run --manifest-path xtask/Cargo.toml --target-dir build/xtask -- doc-csharp

# Live-preview the design docs at http://localhost:3000
[group('docs')]
doc-serve:
    mdbook serve docs --open

# Stage the built docs into build/site (build/site/book + build/site/doc) — the same assembly
# pages.yml publishes. Run `just doc` first. Logic lives in xtask.
[group('docs')]
[doc('Stage the built docs into build/site (what pages.yml publishes)')]
[working-directory: 'xtask']
docs-assemble:
    cargo run -- docs-assemble

# ── Quality gates (also enforced in CI) ──────────────────────────────────

# Rust line coverage (cargo-llvm-cov). CI gates with --fail-under-lines.
[group('quality')]
[working-directory: 'engine']
cov:
    cargo llvm-cov --workspace --summary-only

# License / ban / source policy (cargo-deny). Advisories live in cargo-audit.
[group('quality')]
[working-directory: 'engine']
deny:
    cargo deny check bans licenses sources

# Unused dependencies (cargo-machete).
[group('quality')]
machete:
    cargo machete engine

# Mutation testing (Rust, ADR-0022): which tests pass even when code is broken?
# Slow — scope it, e.g. `just mutants -p fmf-core -f src/query/exec.rs`.
# `just mutants --list -f <file>` enumerates mutants without running them.
[group('quality')]
[doc('Mutation testing (Rust, ADR-0022) — slow; scope it')]
[working-directory: 'engine']
mutants *args="":
    cargo mutants {{args}}

# Mutation testing (C#, Stryker.NET — ADR-0022). Runs FROM the test project dir
# so Stryker discovers FindMyFiles.Tests.csproj and auto-loads stryker-config.json
# (the curated mutate scope) — run from the repo root it errors "no .csproj found".
# Slow on the WinUI app — scope a single file with --mutate, e.g.
# `just stryker --mutate "**/Services/ShellOps.cs"`. The tool is pinned in
# .config/dotnet-tools.json (found by walking up); `dotnet tool restore` provisions it.
[group('quality')]
[doc('Mutation testing (C#, Stryker.NET; ADR-0022)')]
[working-directory: 'app/FindMyFiles.Tests']
stryker *args="":
    dotnet tool restore
    dotnet stryker {{args}}
