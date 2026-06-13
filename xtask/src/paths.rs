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

/// The distributable bundle directory assembled by `publish`.
pub fn dist_dir() -> PathBuf {
    repo_root().join("dist").join("FindMyFiles")
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
