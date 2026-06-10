# find-my-files task runner. Requires: mise (rust/dotnet), see mise.toml.
# Engine recipes work in any shell; app recipes (M1+) may require an elevated terminal.

default: test

# One-time setup: install pinned toolchain + git hooks
setup:
    mise install
    lefthook install

[working-directory: 'engine']
build:
    cargo build --release

[working-directory: 'engine']
test:
    cargo test --workspace

# Fast daily iteration: type-check without codegen (clippy runs on pre-push)
[working-directory: 'engine']
check:
    cargo check --workspace --all-targets

# Elevation-gated #[ignore] tests (real-volume MFT/USN) — run from an elevated terminal
[working-directory: 'engine']
test-admin:
    FMF_ADMIN_TESTS=1 cargo test --workspace -- --ignored

# C# unit tests for the app (no elevation; never rebuilds the Rust engine)
test-app:
    dotnet test app/FindMyFiles.Tests -p:SkipRustBuild=true

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

# Index a volume and print stats (requires elevated terminal)
[working-directory: 'engine']
index drive="C:":
    cargo run --release -p fmf-cli -- index {{drive}} --stats

# Run the benchmark suite against a real volume (requires elevated terminal)
[working-directory: 'engine']
bench drive="C:" *args="":
    cargo run --release -p fmf-cli -- bench {{drive}} {{args}}

# Regression gate vs the committed baseline (requires elevated terminal)
[working-directory: 'engine']
bench-check drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}} --baseline benches/baseline.json

# (Re)record the committed real-volume baseline (requires elevated terminal).
# Note the machine (CPU, entry count) in benches/README when regenerating.
[working-directory: 'engine']
bench-baseline drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}} --json benches/baseline.json

# Criterion micro-benchmarks on a synthetic 1M-entry index (no elevation)
[working-directory: 'engine']
bench-micro *args="":
    cargo bench -p fmf-core {{args}}

# Record the local criterion baseline — run at the start of an optimization
# session. Lives in target/criterion (machine-local, gone on cargo clean).
# (--bench search: criterion-only flags would crash the libtest harness)
[working-directory: 'engine']
bench-micro-baseline:
    cargo bench -p fmf-core --bench search -- --save-baseline committed

# Compare micro-benchmarks against the local baseline; fail on >10% median
# regressions (criterion itself never sets an exit code)
[working-directory: 'engine']
bench-micro-check:
    cargo bench -p fmf-core --bench search -- --baseline committed
    cargo run --release -p fmf-cli -- criterion-gate

# Full performance gate — run from an elevated terminal before merging
# fmf-core changes (real-volume 20% gate + micro-bench 10% gate)
perf-gate: bench-check bench-micro-check

# Profile fmf-cli under samply (ETW — run from an elevated terminal), e.g.
# `just profile bench C:` / `just profile index C: --stats`. Opens the
# Firefox Profiler UI; machine code is identical to release.
[working-directory: 'engine']
profile *args="bench C:":
    cargo build --profile profiling -p fmf-cli
    samply record -- ./target/profiling/fmf-cli {{args}}

# Per-column memory accounting for a real volume (requires elevated terminal)
[working-directory: 'engine']
stats drive="C:":
    cargo run --release -p fmf-cli -- stats {{drive}}
