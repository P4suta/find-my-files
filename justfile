# find-my-files task runner. Requires: mise (rust/dotnet), see mise.toml.
# Recipes marked (elevated) need an administrator terminal.

# just defaults to `sh` even on Windows — absent in elevated PowerShell,
# exactly where the admin recipes must run. powershell.exe always exists.
set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

default:
    @just --list --unsorted

# ── Setup ────────────────────────────────────────────────────────────────

# One-time setup: install pinned toolchain + git hooks
setup:
    mise install
    lefthook install

# ── Daily loop ───────────────────────────────────────────────────────────

# Type-check without codegen — the fast inner loop
[working-directory: 'engine']
check:
    cargo check --workspace --all-targets

[working-directory: 'engine']
build:
    cargo build --release

[working-directory: 'engine']
test:
    cargo test --workspace

# C# unit tests (no elevation; never rebuilds the Rust engine)
test-app:
    dotnet test app/FindMyFiles.Tests -p:SkipRustBuild=true

# C# unit tests + coverage gate (line+branch >=55; UI is [ExcludeFromCodeCoverage]).
# Threshold/type/stat live in the test csproj (with ExcludeByFile), so this is just
# -p:CollectCoverage=true and no comma-bearing prop ever reaches the shell. CI runs
# `just test-app-cov true` (locked enforces packages.lock.json); locally the bare
# recipe reproduces the identical gate.
test-app-cov locked="false":
    dotnet test app/FindMyFiles.Tests -p:SkipRustBuild=true -p:RestoreLockedMode={{locked}} -p:CollectCoverage=true

# Elevation-gated #[ignore] tests: real-volume MFT/USN (elevated).
# Sets the gate env var the PowerShell way on ONE line (just runs each recipe line
# in its own shell, so the assignment must share the line with cargo). `cargo
# --config 'env.X="1"'` is NOT used: the recipe's powershell.exe strips the nested
# quotes, leaving the bare integer 1, which cargo rejects ("expected a string").
[working-directory: 'engine']
test-admin:
    $env:FMF_ADMIN_TESTS = '1'; cargo test --workspace -- --ignored

[working-directory: 'engine']
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    typos

# Format Rust (engine + xtask workspaces) and all TOML (repo-wide, taplo.toml).
fmt:
    cargo fmt --manifest-path engine/Cargo.toml --all
    cargo fmt --manifest-path xtask/Cargo.toml --all
    taplo fmt

# Verify Rust + TOML formatting. C# style/format/analyzers are enforced by the
# build itself (EnforceCodeStyleInBuild + AnalysisMode=All + warnings-as-errors),
# exercised by `test-app` — so `verify` below also covers C#.
fmt-check:
    cargo fmt --manifest-path engine/Cargo.toml --all -- --check
    cargo fmt --manifest-path xtask/Cargo.toml --all -- --check
    taplo fmt --check

# Everything the pre-push hook checks, in one shot
verify: fmt-check lint test test-app

# Regenerate app/FindMyFiles/Engine/Generated/EngineContract.g.cs from the
# contract single source (ADR-0018). cargo test runs the drift check.
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
[working-directory: 'xtask']
publish-app skip_rust="false":
    cargo run --release -- publish --skip-rust {{skip_rust}}

# Local/release publish: build the engine first, then publish (rust is already
# built, so the in-build cargo step is skipped).
publish: build (publish-app "true")

# ── Service (v2: fmf-service + named pipe; ADR-0016/0017) ────────────────

# Console-mode service in the foreground — the dev inner loop (elevated;
# Ctrl+C = flush + graceful stop). Unelevated pipe debugging: add --no-index
[working-directory: 'engine']
service-dev *args="":
    cargo run --release -p fmf-service -- run {{args}}

[working-directory: 'engine']
service-build:
    cargo build --release -p fmf-service

# Register the Windows service: captures your SID, hardens the data-dir
# DACLs, delayed auto start + crash recovery (elevated)
service-install: service-build
    build/engine/release/fmf-service.exe install

# Deregister; data stays unless you pass --purge-data (elevated)
service-uninstall *args="":
    build/engine/release/fmf-service.exe uninstall {{args}}

# (elevated)
service-start:
    build/engine/release/fmf-service.exe start

# (elevated)
service-stop:
    build/engine/release/fmf-service.exe stop

# Rebuild + restart the installed service (elevated)
service-restart: service-stop service-build service-start

# SCM state + live pipe handshake (works unelevated)
service-status:
    build/engine/release/fmf-service.exe status

# C# client × real fmf-service integration (FMF_PIPE_TESTS gate; no elevation)
test-pipe: service-build
    dotnet test app/FindMyFiles.Tests --settings app/FindMyFiles.Tests/pipe.runsettings -p:SkipRustBuild=true

# winapp UI-automation smoke suite (no elevation). Publishes the bundle, then
# hands the published FindMyFiles.exe to ui-tests.ps1, which launches it under
# --engine=empty (setup screen) and --fake-engine (search) and asserts on the
# AutomationIds. The script owns process lifecycle; this recipe is a thin
# pwsh wrapper. -IncludeFaults requires a DEBUG bundle, so it is off here.
ui-test: publish
    pwsh -NoProfile -ExecutionPolicy Bypass -File app/FindMyFiles.Tests/UiAutomation/ui-tests.ps1 -ExePath build/dist/FindMyFiles/FindMyFiles.exe

# ── Benchmarks & gates (discipline: ADR-0013, engine/benches/README.md) ──

# Run the benchmark query set against a real volume (elevated)
[working-directory: 'engine']
bench drive="C:" *args="":
    cargo run --release -p fmf-cli -- bench {{drive}} {{args}}

# Real-volume regression gate vs the committed baseline (elevated, cool machine)
[working-directory: 'engine']
bench-check drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}} --baseline benches/baseline.json

# Note the machine and entry count in benches/README.md when regenerating.
# (Re)record the committed real-volume baseline (elevated, cool machine)
[working-directory: 'engine']
bench-baseline drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}} --json benches/baseline.json

# Criterion micro-benchmarks on the synthetic 1M index (no elevation)
[working-directory: 'engine']
bench-micro *args="":
    cargo bench -p fmf-core {{args}}

# Lives in build/engine/criterion (machine-local; gone on cargo clean).
# Record the local criterion baseline — start of every optimization session
[working-directory: 'engine']
bench-micro-baseline:
    cargo bench -p fmf-core --bench search -- --save-baseline committed

# Compare against the local baseline; fail on >10% median regressions
[working-directory: 'engine']
bench-micro-check:
    cargo bench -p fmf-core --bench search -- --baseline committed
    cargo run --release -p fmf-cli -- criterion-gate --dir ../build/engine/criterion

# Full performance gate before merging fmf-core changes (elevated, cool machine)
perf-gate: bench-check bench-micro-check

# ── Volume tools (elevated) ──────────────────────────────────────────────

# Index a volume, print scan stats, drop into the query REPL
[working-directory: 'engine']
index drive="C:":
    cargo run --release -p fmf-cli -- index {{drive}} --stats

# Per-column memory accounting (the B/entry RAM gate figure)
[working-directory: 'engine']
stats drive="C:" *args="":
    cargo run --release -p fmf-cli -- stats {{drive}} {{args}}

# Name/size distribution — the input for pool/column layout decisions
[working-directory: 'engine']
name-stats drive="C:":
    cargo run --release -p fmf-cli -- stats {{drive}} --name-stats

# $MFT read-throughput probe per I/O strategy (verdicts: ADR-0011)
[working-directory: 'engine']
io-probe drive="C:" mode="buffered" *args="":
    cargo run --release -p fmf-cli -- io-probe {{drive}} --mode {{mode}} {{args}}

# Machine code is identical to release — only debuginfo is upgraded.
# Profile fmf-cli under samply (ETW; elevated), e.g. `just profile bench C:`
[working-directory: 'engine']
profile *args="bench C:":
    cargo build --profile profiling -p fmf-cli
    samply record -- ../build/engine/profiling/fmf-cli {{args}}

# ── Hygiene ──────────────────────────────────────────────────────────────

# Sweep leftover TestDir fixtures (build/engine/test-tmp). Their Drop-time
# removal is best-effort, so killed test runs can leave directories behind;
# cargo clean also removes them, this is the cheaper broom.
[working-directory: 'xtask']
clean-temp:
    cargo run -- clean-temp

# ── Release ──────────────────────────────────────────────────────────────

# Cut a release: bump the version (Rust workspace + C# app in lockstep),
# commit, and create a signed vX.Y.Z tag. Pushing the tag fires release.yml
# (GITHUB_TOKEN only — no stored secret). Logic + guards (semver, existing-tag,
# no-op) + tests live in xtask; `--dry-run` shows the diff without committing.
# Usage:  just release 0.2.0   then   git push; git push origin v0.2.0
[working-directory: 'xtask']
release version *args="":
    cargo run -- release {{version}} {{args}}

# Zip + checksum the assembled bundle for a release tag (run AFTER publish +
# signing). Outputs find-my-files-v<version>-win-x64.zip + SHA256SUMS.txt under
# build/package/ — the assets release.yml attaches. --release: deflate wants a
# non-debug build. Usage:  just package v0.2.0
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
doc: doc-csharp
    mdbook build docs
    cargo doc --no-deps --workspace --document-private-items --manifest-path engine/Cargo.toml --target-dir build/engine

# Build the C# API reference (build/docs-csharp/_site) with DocFX. Metadata comes
# from the BUILT FindMyFiles.dll + its XML doc file, not the .csproj — DocFX
# cannot open the WinUI/WindowsAppSDK project in a Roslyn workspace. The plain
# `dotnet build` (no SkipRustBuild) lets the csproj build fmf_engine.dll itself,
# so this recipe is self-sufficient. docfx is a pinned dotnet local tool.
doc-csharp:
    dotnet build app/FindMyFiles -c Release
    dotnet tool restore
    dotnet docfx docfx/docfx.json

# Live-preview the design docs at http://localhost:3000
doc-serve:
    mdbook serve docs --open

# Stage the built docs into build/site (build/site/book + build/site/doc) — the same assembly
# pages.yml publishes. Run `just doc` first. Logic lives in xtask.
[working-directory: 'xtask']
docs-assemble:
    cargo run -- docs-assemble

# ── Quality gates (also enforced in CI) ──────────────────────────────────

# Rust line coverage (cargo-llvm-cov). CI gates with --fail-under-lines.
[working-directory: 'engine']
cov:
    cargo llvm-cov --workspace --summary-only

# License / ban / source policy (cargo-deny). Advisories live in cargo-audit.
[working-directory: 'engine']
deny:
    cargo deny check bans licenses sources

# Unused dependencies (cargo-machete).
machete:
    cargo machete engine

# Mutation testing (Rust, ADR-0022): which tests pass even when code is broken?
# Slow — scope it, e.g. `just mutants -p fmf-core -f src/query/exec.rs`.
# `just mutants --list -f <file>` enumerates mutants without running them.
[working-directory: 'engine']
mutants *args="":
    cargo mutants {{args}}

# Mutation testing (C#, Stryker.NET — ADR-0022). Slow on the WinUI app — scope
# with --mutate, e.g. `just stryker --mutate "**/ShellOps.cs"`. The tool is
# pinned in .config/dotnet-tools.json; `dotnet tool restore` provisions it.
stryker *args="":
    dotnet tool restore
    dotnet stryker {{args}}
