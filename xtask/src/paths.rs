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

/// The distributable bundle directory assembled by `publish`.
pub fn dist_dir() -> PathBuf {
    build_root().join("dist").join("FindMyFiles")
}

/// Where `package` drops the release zip + `SHA256SUMS.txt`.
pub fn package_dir() -> PathBuf {
    build_root().join("package")
}

/// Where `docs-assemble` stages the GitHub Pages site (`build/site/{book,doc}`).
pub fn site_dir() -> PathBuf {
    build_root().join("site")
}

/// The Rust workspace manifest carrying the release version.
pub fn engine_cargo_toml() -> PathBuf {
    repo_root().join("engine").join("Cargo.toml")
}

/// The C# app project carrying the release version.
pub fn app_csproj() -> PathBuf {
    repo_root()
        .join("app")
        .join("FindMyFiles")
        .join("FindMyFiles.csproj")
}

/// The mise tool-pin manifest at the repo root (what `just doctor` checks).
pub fn mise_toml() -> PathBuf {
    repo_root().join("mise.toml")
}
