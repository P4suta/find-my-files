# xtask

Build/release plumbing for find-my-files, as the [cargo-xtask] pattern: the
imperative logic that used to be inline PowerShell in `justfile` and the GitHub
workflows, rewritten as plain, unit-tested Rust.

`just` is the entry point; it calls in via
`cargo run --manifest-path xtask/Cargo.toml -- <cmd>`.

| Command | `just` recipe | What it does |
|---|---|---|
| `version --channel <c> [--date]` | `just version …` | Print the channel-aware build version (`dev`/`nightly`/`stable`) — the source of the `FMF_BUILD_VERSION` format. Versioning itself is release-please's job (ADR-0035), not xtask's. |
| `publish [--skip-rust <bool>]` | `just publish` / `just publish-app <skip>` | Clean → `dotnet publish` → prune unshipped locale dirs → copy engine binaries → **self-verify** the bundle is runnable. |
| `package [<tag>]` | `just package [<tag>]` | Zip + SHA256 the bundle into `build/package/`. With a `vX.Y.Z` tag → stable name; omit it for a nightly (named from `FMF_BUILD_VERSION`). Run after signing. |
| `clean-temp` | `just clean-temp` | Sweep leftover test fixtures (`build/engine/test-tmp`). |

## Why a separate workspace

This is its OWN single-crate workspace, deliberately **not** a member of
`engine/`. The daily loop and CI gates run `cargo {check,clippy,doc} --workspace`
and `cargo llvm-cov --workspace --fail-under-lines 70` over `engine/`; a member
here would pollute the coverage denominator and the lint/doc surface.

## Testing

```sh
cargo test     --manifest-path xtask/Cargo.toml
cargo clippy   --manifest-path xtask/Cargo.toml --all-targets -- -D warnings
cargo fmt      --manifest-path xtask/Cargo.toml --check
```

The pure logic — the channel version formatter (`version.rs`), the locale-prune
predicate, checksums, semver — is unit-tested; the I/O commands are exercised end-to-end by
CI (`app` job runs `just publish-app`, `release.yml` runs `just publish` +
`just package`). CI gates this crate in the ubuntu `xtask` job; advisories are
scanned in `cargo-audit.yml`.

[cargo-xtask]: https://github.com/matklad/cargo-xtask
