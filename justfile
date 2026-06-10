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
bench drive="C:":
    cargo run --release -p fmf-cli -- bench {{drive}}
