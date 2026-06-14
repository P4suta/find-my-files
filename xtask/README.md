# xtask

Build/release plumbing for find-my-files, as the [cargo-xtask] pattern: the
imperative logic that used to be inline PowerShell in `justfile` and the GitHub
workflows, rewritten as plain, unit-tested Rust.

`just` is the entry point; it calls in via
`cargo run --manifest-path xtask/Cargo.toml -- <cmd>`.

| Command | `just` recipe | What it does |
|---|---|---|
| `release <ver> [--dry-run]` | `just release <ver>` | Bump the version (Rust workspace + C# app in lockstep), commit, signed tag. Guards: semver, existing-tag, no-op. |
| `publish [--skip-rust <bool>]` | `just publish` / `just publish-app <skip>` | Clean → `dotnet publish` → prune unshipped locale dirs → copy engine binaries → **self-verify** the bundle is runnable. |
| `package <tag>` | `just package <tag>` | Zip + SHA256 the bundle into `build/package/find-my-files-v<ver>-win-x64.zip` + `SHA256SUMS.txt`. Run after signing. |
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

The pure logic — version rewriting (`toml_edit`), the locale-prune predicate,
checksums, semver — is unit-tested; the I/O commands are exercised end-to-end by
CI (`app` job runs `just publish-app`, `release.yml` runs `just publish` +
`just package`). CI gates this crate in the ubuntu `xtask` job; advisories are
scanned in `cargo-audit.yml`.

[cargo-xtask]: https://github.com/matklad/cargo-xtask
