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

/// Criterion's canonical machine-local baseline tree. Comparison runs are
/// staged elsewhere and seeded from this tree, so stale `new`/`change` reports
/// can never satisfy the regression gate. Dedicated runners set
/// `FMF_PERF_BASELINE_DIR` to an existing persistent directory outside the
/// checkout; local development keeps the normal single `build/` output tree.
pub fn criterion_dir() -> PathBuf {
    std::env::var_os("FMF_PERF_BASELINE_DIR").map_or_else(
        || build_root().join("engine").join("criterion"),
        PathBuf::from,
    )
}

/// Transactional performance-run scratch space under the canonical build tree.
pub fn perf_dir() -> PathBuf {
    build_root().join("engine").join("perf")
}

/// Deterministic mutation-testing evidence. Tool-native reports and the
/// canonical gate summaries stay below this one ignored build subtree.
pub fn mutation_dir() -> PathBuf {
    build_root().join("mutation")
}

pub fn rust_mutation_dir() -> PathBuf {
    mutation_dir().join("rust")
}

pub fn csharp_mutation_dir() -> PathBuf {
    mutation_dir().join("csharp")
}

/// Reviewed exact-equivalent survivor set for cargo-mutants.
pub fn rust_mutation_baseline() -> PathBuf {
    engine_dir().join("mutation-baseline.json")
}

/// Reviewed exact-equivalent survivor set for Stryker.NET.
pub fn csharp_mutation_baseline() -> PathBuf {
    repo_root()
        .join("app")
        .join("FindMyFiles.Tests")
        .join("mutation-baseline.json")
}

/// The committed real-volume performance baseline.
pub fn real_baseline() -> PathBuf {
    engine_dir().join("benches").join("baseline.json")
}

/// The distributable bundle directory assembled by `publish` — the zip root.
/// Holds only the native launcher (`FindMyFiles.exe`) + `README.txt`; the
/// self-contained app lives one level down in [`app_dir`].
pub fn dist_dir() -> PathBuf {
    build_root().join("dist").join("FindMyFiles")
}

/// Exact deterministic manifest for the unsigned distribution tree. It is a
/// sibling of (never a member of) [`dist_dir`], so sealing does not mutate the
/// tree whose identity it records.
pub fn unsigned_bundle_manifest() -> PathBuf {
    build_root()
        .join("dist")
        .join("FindMyFiles.unsigned.manifest.json")
}

/// Exact deterministic manifest for the signed distribution candidate.
pub fn signed_bundle_manifest() -> PathBuf {
    build_root()
        .join("dist")
        .join("FindMyFiles.signed.manifest.json")
}

/// The self-contained app payload, one level under the bundle root. The ~100
/// publish files (apphost, runtime DLLs, engine binaries) stay co-located here
/// because the .NET apphost resolves its DLLs / `*.deps.json` from its own
/// directory — so only the launcher + README can sit at the root.
pub fn app_dir() -> PathBuf {
    dist_dir().join("app")
}

/// Deterministic `CycloneDX` documents generated from the final distribution
/// tree. Release/nightly publish exactly the two top-level `*.cdx.json` files
/// produced here.
pub fn sbom_dir() -> PathBuf {
    build_root().join("sbom")
}

/// `NuGet`'s resolved restore graph for the shipping app. `obj/` intentionally
/// stays beside the project (ADR-0021); SBOM generation reads this machine
/// output only after `dotnet publish --locked-mode` has completed.
pub fn app_project_assets() -> PathBuf {
    repo_root()
        .join("app")
        .join("FindMyFiles")
        .join("obj")
        .join("project.assets.json")
}

/// A separately compiled publish tree with deterministic UI-test seams enabled.
/// It can never be confused with or packaged from [`dist_dir`].
pub fn ui_test_dist_dir() -> PathBuf {
    build_root().join("ui-test-bundle").join("FindMyFiles")
}

pub fn ui_test_app_dir() -> PathBuf {
    ui_test_dist_dir().join("app")
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

/// The committed manifest of first-party PE bundle paths the release
/// `verify-signatures` composite action checks. Generated from
/// [`crate::publish::FIRST_PARTY_PES`] and pinned by a drift test, so the signed-
/// file list lives in exactly one place (xtask) instead of a hardcoded copy in
/// the action's PowerShell. The action reads the file directly from the checkout
/// (via `GITHUB_ACTION_PATH`); xtask only needs this path to pin/bless it, hence
/// test-only.
#[cfg(test)]
pub fn signed_pe_manifest() -> PathBuf {
    repo_root()
        .join(".github")
        .join("actions")
        .join("verify-signatures")
        .join("first-party-pes.txt")
}

/// Where `docs-assemble` stages the landing page and canonical book.
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
