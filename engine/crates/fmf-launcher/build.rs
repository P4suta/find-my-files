//! Embeds the application icon AND a channel-aware version resource into the
//! launcher executable, so the bundle's top-level `FindMyFiles.exe` is both
//! visually identical to the app it starts and identifiable in Explorer →
//! Properties → Details (ProductName/ProductVersion) without running anything.
//!
//! Best-effort by design: if the resource compiler is unavailable the build
//! still succeeds (the launcher just keeps the default icon/strings). A cosmetic
//! must never break the build — and CI (windows-latest, full SDK) always has the
//! compiler, so the shipped release artifact gets the resource regardless.
//!
//! The version string follows the SAME precedence as the fmf-buildstamp build.rs
//! (env `FMF_BUILD_VERSION` verbatim, else the local `…-dev+g<sha>` default), so
//! the launcher's reported version never disagrees with `fmf --version`. The
//! ~5-line dev fallback below intentionally mirrors `fmf-buildstamp/build.rs`;
//! the format authority remains `xtask version`.

use std::process::Command;

fn main() {
    // The canonical icon is the app's own, referenced directly to avoid a
    // second copy that could drift. Path is relative to this crate dir
    // (engine/crates/fmf-launcher), which is build.rs's working directory.
    const ICON: &str = "../../../app/FindMyFiles/Assets/AppIcon.ico";
    println!("cargo:rerun-if-changed={ICON}");
    // The version resource must be re-stamped when the build identity moves.
    println!("cargo:rerun-if-env-changed=FMF_BUILD_VERSION");
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/index");

    // Resources are a Windows-only concept; the whole project is Windows-only,
    // but guard anyway so a non-Windows `cargo check` stays clean.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let full = resolve_version();

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON);
    // Override winresource's Cargo-derived defaults (which would read as the
    // internal crate `fmf-launcher` @ a static `0.1.0.0`): present the product
    // name and the channel-aware version a downloader actually cares about.
    res.set("ProductName", "FindMyFiles");
    res.set(
        "FileDescription",
        "FindMyFiles — fast filename search for Windows",
    );
    res.set("ProductVersion", full.as_str());
    res.set("OriginalFilename", "FindMyFiles.exe");
    res.set("LegalCopyright", "Apache-2.0");
    res.set("Comments", "https://github.com/P4suta/find-my-files");
    // Invariant: the numeric FIXEDFILEINFO below and the string `ProductVersion`
    // above share the SAME `X.Y.Z` base (CARGO_PKG_VERSION = the release-please
    // workspace version). The numeric form is the clean base only (Win32 digits
    // can't carry a channel/sha/.dirty); the string carries the full channel-aware
    // identity. They never disagree on the base triple.
    if let Some(v) = numeric_version(env!("CARGO_PKG_VERSION")) {
        res.set_version_info(winresource::VersionInfo::FILEVERSION, v);
        res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, v);
    }
    if let Err(e) = res.compile() {
        println!("cargo:warning=fmf-launcher: version resource not embedded ({e})");
    }
}

/// Channel-aware build version, mirroring `fmf-buildstamp/build.rs` precedence:
/// `FMF_BUILD_VERSION` (CI authoritative) else the local `…-dev+g<sha>[.dirty]`
/// default. The `.dirty` suffix sits next to the sha (never on the no-git path),
/// byte-identical to what `fmf-buildstamp` stamps, so a dirty dev build reports
/// the same version across the launcher, the binaries, and the bundle.
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

/// Pack an `X.Y.Z` base version into the u64 winresource expects for the numeric
/// FILEVERSION/PRODUCTVERSION (each component a u16: `major.minor.patch.0`).
fn numeric_version(base: &str) -> Option<u64> {
    let mut it = base.split('.');
    let major: u64 = it.next()?.parse().ok()?;
    let minor: u64 = it.next()?.parse().ok()?;
    let patch: u64 = it.next()?.parse().ok()?;
    Some((major << 48) | (minor << 32) | (patch << 16))
}
