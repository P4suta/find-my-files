//! Resolve the channel-aware build version once, at compile time, and inject it
//! via `rustc-env` so `src/lib.rs` can expose it as a `const`.
//!
//! Precedence:
//!   1. `FMF_BUILD_VERSION` (CI authoritative — nightly/stable set it verbatim,
//!      see `xtask version --channel …`).
//!   2. `{CARGO_PKG_VERSION}-dev+g{sha}[.dirty]` — the local contributor default,
//!      so a hand-built binary is never mistaken for an official release.
//!
//! `rerun-if-changed` is scoped to the git refs that actually move the answer, so
//! the daily edit→build loop on a single commit never re-runs this script. When
//! `.git` is absent (source tarball) the sha is simply omitted.

use std::process::Command;

fn main() {
    // Only HEAD/index movement changes the stamp; pure source edits on one commit
    // leave these untouched, so the script stays cached during the inner loop.
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/index");
    println!("cargo:rerun-if-env-changed=FMF_BUILD_VERSION");

    println!("cargo:rustc-env=FMF_VERSION_STRING={}", resolve_version());
}

fn resolve_version() -> String {
    if let Ok(forced) = std::env::var("FMF_BUILD_VERSION") {
        let forced = forced.trim();
        if !forced.is_empty() {
            return forced.to_owned();
        }
    }

    // The base triple is the release-please-managed workspace version.
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

fn git_is_dirty() -> bool {
    // Best-effort: evaluated only when HEAD/index moved (rerun gating above), so a
    // build, then unstaged edit, then rebuild may not flip the flag — the sha still
    // pins the base commit. `git status --porcelain` prints one line per change.
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}
