//! Path anchoring. Everything resolves from the repo root, which is the parent
//! of the xtask crate dir baked in at compile time — so the commands behave the
//! same regardless of the caller's working directory.

use std::path::{Path, PathBuf};

/// The repository root (parent of `xtask/`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a parent (the repo root)")
        .to_path_buf()
}

/// The single build-output tree: every artifact (cargo target dirs, the C# bin
/// output, the publish bundle, release packages, the staged docs site) lives
/// under `<repo>/build/` (ADR-0021), gitignored as one line.
pub fn build_root() -> PathBuf {
    repo_root().join("build")
}

/// The engine workspace's release artifacts (`build/engine/release`), where the
/// engine `.cargo/config.toml` redirects `cargo build --release` output.
pub fn engine_release_dir() -> PathBuf {
    build_root().join("engine").join("release")
}

/// The distributable bundle directory assembled by `publish` — the zip root.
/// Holds only the native launcher (`FindMyFiles.exe`) + `README.txt`; the
/// self-contained app lives one level down in [`app_dir`].
pub fn dist_dir() -> PathBuf {
    build_root().join("dist").join("FindMyFiles")
}

/// The self-contained app payload, one level under the bundle root. The ~100
/// publish files (apphost, runtime DLLs, engine binaries) stay co-located here
/// because the .NET apphost resolves its DLLs / `*.deps.json` from its own
/// directory — so only the launcher + README can sit at the root.
pub fn app_dir() -> PathBuf {
    dist_dir().join("app")
}

/// Where `package` drops the release zip + `SHA256SUMS.txt`.
pub fn package_dir() -> PathBuf {
    build_root().join("package")
}

/// Flat staging dir the release signing step feeds to the eSigner Action
/// (`sign-stage` populates it, one uniquely-named copy per first-party PE).
/// Under `build/` so it is covered by the single ignore line (ADR-0021); the
/// workflow points the Action at the matching `build\sign-stage`.
pub fn sign_stage_dir() -> PathBuf {
    build_root().join("sign-stage")
}

/// Dir the eSigner Action writes the signed PEs into (by their stage names);
/// `sign-collect` copies them back over the bundle. Under `build/` to match
/// [`sign_stage_dir`] and the workflow's `build\signed`.
pub fn signed_dir() -> PathBuf {
    build_root().join("signed")
}

/// Where `docs-assemble` stages the GitHub Pages site (`build/site/{book,doc}`).
pub fn site_dir() -> PathBuf {
    build_root().join("site")
}

/// The engine workspace dir. Running `cargo` from here (not `--manifest-path`
/// from the root) is what lets its `.cargo/config.toml` redirect the target dir
/// under `build/` — the same reason the just recipes use `[working-directory:
/// 'engine']`.
pub fn engine_dir() -> PathBuf {
    repo_root().join("engine")
}

/// The Rust workspace manifest carrying the base version (`xtask version` reads
/// the `[workspace.package] version` here; release-please bumps it).
pub fn engine_cargo_toml() -> PathBuf {
    repo_root().join("engine").join("Cargo.toml")
}

/// The mise tool-pin manifest at the repo root (what `just doctor` checks).
pub fn mise_toml() -> PathBuf {
    repo_root().join("mise.toml")
}
