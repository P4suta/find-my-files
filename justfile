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

# Per-column memory accounting for a real volume (requires elevated terminal)
[working-directory: 'engine']
stats drive="C:":
    cargo run --release -p fmf-cli -- stats {{drive}}
