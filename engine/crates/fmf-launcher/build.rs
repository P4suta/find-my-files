//! Embeds the application icon AND a channel-aware version resource into the launcher executable, so the bundle's top-level `FindMyFiles.exe` is both visually identical to the app it starts and identifiable in Explorer → Properties → Details (ProductName/ProductVersion) without running anything.
//!
//! Resource embedding is a release invariant, not cosmetic: Explorer identity and the application icon must be present on the shipped launcher.
//! A local Windows debug build warns when the resource compiler is unavailable, but a release-profile or CI build fails closed.
//! Non-Windows checks remain a no-op.
//!
//! The version string follows the SAME precedence as the fmf-buildstamp build.rs (env `FMF_BUILD_VERSION` verbatim, else the local `…-dev+g<sha>` default), so the launcher's reported version never disagrees with `fmf --version`.
//! The ~5-line dev fallback below intentionally mirrors `fmf-buildstamp/build.rs`;
//! the format authority remains `xtask version`.

use std::process::Command;

fn main() {
    const ICON: &str = "../../../app/FindMyFiles/Assets/AppIcon.ico";
    const PROJECT_URL: &str = "https://github.com/P4suta/find-my-files";
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-env-changed=FMF_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=CI");
    println!("cargo:rerun-if-env-changed=GITHUB_ACTIONS");
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/index");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let full = resolve_version();

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON);
    res.set("ProductName", "FindMyFiles");
    res.set(
        "FileDescription",
        "FindMyFiles — fast filename search for Windows",
    );
    res.set("ProductVersion", full.as_str());
    res.set("OriginalFilename", "FindMyFiles.exe");
    res.set("LegalCopyright", "Apache-2.0");
    res.set("Comments", PROJECT_URL);
    // Invariant: the numeric FIXEDFILEINFO below and the string `ProductVersion`
    if let Some(v) = numeric_version(env!("CARGO_PKG_VERSION")) {
        res.set_version_info(winresource::VersionInfo::FILEVERSION, v);
        res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, v);
    }
    if let Err(e) = res.compile() {
        assert!(
            !resource_failure_is_fatal(),
            "fmf-launcher: failed to embed required icon/version resource \
             in a release or CI build: {e}"
        );
        println!(
            "cargo:warning=fmf-launcher: icon/version resource not embedded in \
             local debug build ({e})"
        );
    }
}

/// Shipping builds must never silently lose VERSIONINFO/the application icon.
/// Cargo supplies `PROFILE`; the CI variables also make non-release verification jobs fail if their Windows SDK/resource compiler setup regresses.
fn resource_failure_is_fatal() -> bool {
    std::env::var("PROFILE").as_deref() == Ok("release")
        || env_truthy("CI")
        || env_truthy("GITHUB_ACTIONS")
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

/// Channel-aware build version, mirroring `fmf-buildstamp/build.rs` precedence:
/// `FMF_BUILD_VERSION` (CI authoritative) else the local `…-dev+g<sha>[.dirty]` default.
/// The `.dirty` suffix sits next to the sha (never on the no-git path),
/// byte-identical to what `fmf-buildstamp` stamps, so a dirty dev build reports the same version across the launcher, the binaries, and the bundle.
fn resolve_version() -> String {
    if let Ok(forced) = std::env::var("FMF_BUILD_VERSION") {
        let forced = forced.trim();
        if !forced.is_empty() {
            return forced.to_owned();
        }
    }
    let base = env!("CARGO_PKG_VERSION");
    match git_short_sha() {
        Some(sha) => {
            let dirty = if git_is_dirty() { ".dirty" } else { "" };
            format!("{base}-dev+g{sha}{dirty}")
        }
        None => format!("{base}-dev"),
    }
}

fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Best-effort dirty check (`git status --porcelain` prints one line per change),
/// mirroring `fmf-buildstamp/build.rs` so the `.dirty` marker agrees across stampers.
fn git_is_dirty() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Pack an `X.Y.Z` base version into the u64 winresource expects for the numeric FILEVERSION/PRODUCTVERSION (each component a u16: `major.minor.patch.0`).
fn numeric_version(base: &str) -> Option<u64> {
    let mut it = base.split('.');
    let major: u64 = it.next()?.parse().ok()?;
    let minor: u64 = it.next()?.parse().ok()?;
    let patch: u64 = it.next()?.parse().ok()?;
    Some((major << (u16::BITS + u32::BITS)) | (minor << u32::BITS) | (patch << u16::BITS))
}
