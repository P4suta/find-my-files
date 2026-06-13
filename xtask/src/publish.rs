//! `xtask publish [--skip-rust]` — assemble the distributable bundle in
//! dist/FindMyFiles.
//!
//! Publishes the app (not a bare `dotnet build` — only the publish output wires
//! WinRT.Runtime.dll, the `WinAppSDK` native helpers and the compiled XAML into a
//! runnable bundle), prunes the locale dirs the app doesn't ship, copies the
//! engine binaries, then SELF-VERIFIES the result. The self-check is what lets
//! us drop ci.yml's separate "verify bundle is runnable" step: the producer of
//! the bundle guarantees its own output instead of a downstream guard.
//!
//! `--skip-rust true` skips the in-build cargo step (CI prebuilds + downloads
//! the engine binaries into engine/target/release/ before this runs).

use crate::{cmd, fsx, locale, paths};
use anyhow::{bail, Context, Result};
use std::fs;

/// Engine binaries copied in alongside the published app.
const ENGINE_BINS: &[&str] = &["fmf-service.exe", "fmf.exe"];

/// Files whose presence means the bundle can actually launch. `fmf_engine.dll`
/// arrives via the csproj `<None Include>`; the two exes are copied below.
const REQUIRED: &[&str] = &[
    "WinRT.Runtime.dll",
    "fmf_engine.dll",
    "fmf-service.exe",
    "fmf.exe",
];

pub fn run(skip_rust: bool) -> Result<()> {
    let root = paths::repo_root();
    let dist = paths::dist_dir();

    // Clean a stale bundle (read-only ReadyToRun DLLs and all). Best-effort by
    // design: a leftover bundle can be locked by a running app, and `dotnet
    // publish` overwrites anyway — the self-verify at the end is the real gate.
    // We warn rather than fail (the old recipe swallowed this silently).
    if let Err(e) = fsx::force_remove_dir_all(&dist) {
        eprintln!(
            "warning: could not fully clean {} ({e}); publishing over the leftovers",
            dist.display()
        );
    }

    // Publish into dist/FindMyFiles (relative to the repo root).
    let skip_arg = format!("-p:SkipRustBuild={skip_rust}");
    cmd::run(
        &root,
        "dotnet",
        &[
            "publish",
            "app/FindMyFiles",
            "-c",
            "Release",
            "-r",
            "win-x64",
            "-o",
            "dist/FindMyFiles",
            &skip_arg,
        ],
    )?;

    prune_locales(&dist)?;
    copy_engine_bins(&root, &dist)?;
    verify_bundle(&dist)?;

    println!(
        "publish: dist/FindMyFiles assembled and verified ({} required files present).",
        REQUIRED.len()
    );
    Ok(())
}

/// Remove `WinAppSDK` locale dirs the app doesn't ship (lookups fall back to the
/// neutral resources). Collect first, then delete — don't mutate the directory
/// mid-enumeration.
fn prune_locales(dist: &std::path::Path) -> Result<()> {
    let mut to_prune = Vec::new();
    for entry in fs::read_dir(dist).with_context(|| format!("read {}", dist.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if locale::should_prune_locale_dir(&entry.file_name().to_string_lossy()) {
            to_prune.push(entry.path());
        }
    }
    for dir in to_prune {
        fsx::force_remove_dir_all(&dir)
            .with_context(|| format!("prune locale {}", dir.display()))?;
    }
    Ok(())
}

fn copy_engine_bins(root: &std::path::Path, dist: &std::path::Path) -> Result<()> {
    let release = root.join("engine").join("target").join("release");
    for bin in ENGINE_BINS {
        let src = release.join(bin);
        let target = dist.join(bin);
        fs::copy(&src, &target)
            .with_context(|| format!("copy {} -> {}", src.display(), target.display()))?;
    }
    Ok(())
}

fn verify_bundle(dist: &std::path::Path) -> Result<()> {
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|f| !dist.join(f).exists())
        .collect();
    if !missing.is_empty() {
        bail!(
            "bundle at {} is missing {missing:?} — it would not launch",
            dist.display()
        );
    }
    Ok(())
}
