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

# Elevation-gated #[ignore] tests: real-volume MFT/USN (elevated)
[working-directory: 'engine']
test-admin:
    $env:FMF_ADMIN_TESTS='1'; cargo test --workspace -- --ignored

[working-directory: 'engine']
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    typos

[working-directory: 'engine']
fmt:
    cargo fmt --all
    taplo fmt

[working-directory: 'engine']
fmt-check:
    cargo fmt --all -- --check
    taplo check

# Everything the pre-push hook checks, in one shot
verify: fmt-check lint test test-app

# Regenerate app/FindMyFiles/Engine/Generated/EngineContract.g.cs from the
# contract single source (ADR-0018). cargo test runs the drift check.
[working-directory: 'engine']
contract-gen:
    cargo run -p fmf-contract --bin gen-contract

# Clean distributable bundle in dist/FindMyFiles: published app + engine
# binaries (fmf-service.exe / fmf.exe). WinAppSDK ships ~85 locale resource
# dirs; everything but en-us/ja-JP is pruned (lookups fall back to neutral
# resources when a locale dir is absent).
publish: build
    Remove-Item dist/FindMyFiles -Recurse -Force -ErrorAction SilentlyContinue; exit 0
    dotnet publish app/FindMyFiles -c Release -r win-x64 -o dist/FindMyFiles
    Get-ChildItem dist/FindMyFiles -Directory | Where-Object { $_.Name -match '^[a-z]{2,3}(-[A-Za-z0-9]+){1,3}$' -and $_.Name -notin @('en-us','ja-JP') } | Remove-Item -Recurse -Force
    Copy-Item engine/target/release/fmf-service.exe, engine/target/release/fmf.exe dist/FindMyFiles/

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
    engine/target/release/fmf-service.exe install

# Deregister; data stays unless you pass --purge-data (elevated)
service-uninstall *args="":
    engine/target/release/fmf-service.exe uninstall {{args}}

# (elevated)
service-start:
    engine/target/release/fmf-service.exe start

# (elevated)
service-stop:
    engine/target/release/fmf-service.exe stop

# Rebuild + restart the installed service (elevated)
service-restart: service-stop service-build service-start

# SCM state + live pipe handshake (works unelevated)
service-status:
    engine/target/release/fmf-service.exe status

# C# client × real fmf-service integration (FMF_PIPE_TESTS gate; no elevation)
test-pipe: service-build
    $env:FMF_PIPE_TESTS='1'; dotnet test app/FindMyFiles.Tests -p:SkipRustBuild=true

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

# Lives in target/criterion (machine-local; gone on cargo clean).
# Record the local criterion baseline — start of every optimization session
[working-directory: 'engine']
bench-micro-baseline:
    cargo bench -p fmf-core --bench search -- --save-baseline committed

# Compare against the local baseline; fail on >10% median regressions
[working-directory: 'engine']
bench-micro-check:
    cargo bench -p fmf-core --bench search -- --baseline committed
    cargo run --release -p fmf-cli -- criterion-gate

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
    samply record -- ./target/profiling/fmf-cli {{args}}

# ── Hygiene ──────────────────────────────────────────────────────────────

# Sweep leftover TestDir fixtures (engine/target/test-tmp). Their Drop-time
# removal is best-effort, so killed test runs can leave directories behind;
# cargo clean also removes them, this is the cheaper broom.
clean-temp:
    Remove-Item -Recurse -Force engine/target/test-tmp -ErrorAction SilentlyContinue; exit 0

# ── Release ──────────────────────────────────────────────────────────────

# Cut a release: bump the version (Rust workspace + C# app in lockstep),
# commit, and create a signed vX.Y.Z tag. Pushing the tag fires release.yml
# (GITHUB_TOKEN only — no stored secret). -Raw/-NoNewline preserve LF endings.
# Usage:  just release 0.2.0   then   git push; git push origin v0.2.0
release version:
    (Get-Content -Raw engine/Cargo.toml) -replace '\d+\.\d+\.\d+(?=" # release version)', '{{version}}' | Set-Content -NoNewline engine/Cargo.toml
    (Get-Content -Raw app/FindMyFiles/FindMyFiles.csproj) -replace '\d+\.\d+\.\d+(?=</Version> <!-- release version)', '{{version}}' | Set-Content -NoNewline app/FindMyFiles/FindMyFiles.csproj
    git add engine/Cargo.toml app/FindMyFiles/FindMyFiles.csproj
    git commit -m "chore: release v{{version}}"
    git tag -s "v{{version}}" -m "v{{version}}"
    Write-Host "Tagged v{{version}} — push with: git push; git push origin v{{version}}"

# ── Docs ─────────────────────────────────────────────────────────────────

# Build the mdBook design docs (docs/book) + rustdoc (engine/target/doc).
# Same outputs the pages.yml workflow publishes to GitHub Pages.
# --document-private-items: the crates are internal (no external API surface),
# so the docs are for maintainers — private items are the interesting part, and
# documenting them also resolves intra-doc links to non-pub helpers.
doc:
    mdbook build docs
    cargo doc --no-deps --workspace --document-private-items --manifest-path engine/Cargo.toml

# Live-preview the design docs at http://localhost:3000
doc-serve:
    mdbook serve docs --open

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
