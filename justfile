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
