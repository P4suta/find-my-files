//! `xtask version --channel <dev|nightly|stable> [--date YYYYMMDD]` — print the
//! canonical channel-aware version string. This is the single source of the
//! *format*: CI exports the result as `FMF_BUILD_VERSION` so the fmf-buildstamp
//! build.rs stamps it verbatim, and `xtask package` names the nightly zip from it.
//!
//!   dev     → 0.1.0-dev+g<sha>
//!   nightly → 0.1.0-nightly.<date>+g<sha>
//!   stable  → 0.1.0                          (clean; the release tag itself)
//!
//! The base `X.Y.Z` triple is read from engine/Cargo.toml `[workspace.package]
//! version` (the value release-please bumps). The git sha is resolved at call
//! time; when `.git`/git is absent the metadata is simply omitted.
//!
//! Release *bumping* is NOT here — release-please owns the version/tag/CHANGELOG.
//! This subcommand only formats a build identity for the dev/nightly/stable lanes.

use crate::{cmd, paths, semver};
use anyhow::{bail, Context, Result};
use std::fmt::Write as _;
use std::fs;
use toml_edit::DocumentMut;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Dev,
    Nightly,
    Stable,
}

impl Channel {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "dev" => Ok(Self::Dev),
            "nightly" => Ok(Self::Nightly),
            "stable" => Ok(Self::Stable),
            other => bail!("unknown channel '{other}' (expected dev|nightly|stable)"),
        }
    }
}

pub fn run(channel: &str, date: Option<&str>) -> Result<()> {
    let channel = Channel::parse(channel)?;
    let base = workspace_base_version()?;
    let sha = git_short_sha();
    println!("{}", compute(&base, channel, date, sha.as_deref())?);
    Ok(())
}

/// `xtask check-version <tag>`: hard-fail unless the release `tag` (`vX.Y.Z` or
/// `X.Y.Z`) matches the committed `[workspace.package] version`. release.yml runs
/// this on a publishing dispatch BEFORE signing/packaging, so a manual dispatch
/// whose tag drifts from the code (the zip name, the build stamp, and the Release
/// tag all come from that tag) is caught before it ships mislabeled artifacts.
pub fn check_release_tag(tag: &str) -> Result<()> {
    let committed = workspace_base_version()?;
    tag_matches(tag, &committed)
}

/// Pure comparison behind [`check_release_tag`] — unit-tested without the FS.
fn tag_matches(tag: &str, committed: &str) -> Result<()> {
    let want = semver::strip_tag_v(tag);
    if want != committed {
        bail!(
            "release tag '{tag}' (version {want}) does not match the committed \
             workspace version {committed} in engine/Cargo.toml — bump one so they agree"
        );
    }
    Ok(())
}

/// Pure formatter — unit-tested without touching git or the filesystem.
fn compute(base: &str, channel: Channel, date: Option<&str>, sha: Option<&str>) -> Result<String> {
    let meta = sha.map(|s| format!("+g{s}")).unwrap_or_default();
    Ok(match channel {
        Channel::Stable => base.to_owned(),
        Channel::Dev => format!("{base}-dev{meta}"),
        Channel::Nightly => {
            let date = date.context("--date YYYYMMDD is required for the nightly channel")?;
            if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
                bail!("--date must be 8 digits (YYYYMMDD), got '{date}'");
            }
            format!("{base}-nightly.{date}{meta}")
        }
    })
}

/// The channel + commit + date decoded from a build-version string. Pure mirror
/// of the format `compute` (above) and the fmf-buildstamp build.rs produce, so a
/// downloaded bundle can be classified from the stamped string alone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BuildIdentity {
    /// `"dev" | "nightly" | "stable"` — the lane the artifact came from.
    pub channel: &'static str,
    /// The 7-char git sha (may carry a `.dirty` suffix on local builds), if stamped.
    pub commit: Option<String>,
    /// The `YYYYMMDD` build date — present only for nightly.
    pub date: Option<String>,
}

/// Decode a build-version string into its channel/commit/date. Inverse of
/// [`compute`]: `0.1.0` → stable, `0.1.0-dev+g<sha>` → dev, and
/// `0.1.0-nightly.<date>+g<sha>` → nightly. Pure (no git, no FS).
pub fn parse_identity(full: &str) -> BuildIdentity {
    // The git sha is everything after the `+g` build-metadata marker.
    let commit = full.split_once("+g").map(|(_, c)| c.to_owned());
    // The pre-release label sits between the first `-` and the `+` metadata.
    let pre = full
        .split_once('-')
        .map(|(_, rest)| rest.split('+').next().unwrap_or("").to_owned());
    let (channel, date) = match pre.as_deref() {
        None => ("stable", None),
        Some(p) if p.starts_with("nightly.") => ("nightly", Some(p["nightly.".len()..].to_owned())),
        // `dev`, or any unrecognised pre-release, classifies as a non-official
        // (dev) build — never silently mistaken for a release.
        Some(_) => ("dev", None),
    };
    BuildIdentity {
        channel,
        commit,
        date,
    }
}

/// Render the human-and-grep friendly `BUILDINFO.txt` body (LF; the caller adds
/// the BOM/CRLF for Notepad). Pure: `full` is the stamped version and
/// `commit_date` the `git show -s --format=%cs` date (`YYYY-MM-DD`), if known.
/// For nightly the date embedded in the version wins over the commit date.
pub fn render_buildinfo(full: &str, commit_date: Option<&str>) -> String {
    const SOURCE: &str = "https://github.com/P4suta/find-my-files";
    let id = parse_identity(full);
    let date = match (id.channel, &id.date) {
        // Nightly carries its own build date (YYYYMMDD) — reformat to YYYY-MM-DD.
        ("nightly", Some(d)) if d.len() == 8 => {
            Some(format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8]))
        }
        _ => commit_date.map(str::to_owned),
    };
    let mut out = String::new();
    out.push_str("FindMyFiles\n");
    let _ = writeln!(out, "version:  {full}");
    let _ = writeln!(out, "channel:  {}", id.channel);
    if let Some(c) = &id.commit {
        let _ = writeln!(out, "commit:   {c}");
    }
    if let Some(d) = &date {
        let _ = writeln!(out, "date:     {d}");
    }
    let _ = writeln!(out, "source:   {SOURCE}");
    out.push_str("license:  Apache-2.0\n");
    out
}

/// Resolve the build version to stamp into the bundle's `BUILDINFO.txt`, with the
/// SAME precedence the fmf-buildstamp build.rs uses for the binaries: the CI
/// `FMF_BUILD_VERSION` verbatim, else the local `…-dev+g<sha>[.dirty]` default.
/// Keeps the in-file label identical to what the shipped binaries report — a local
/// dirty `just publish` must not have the exes say `.dirty` while BUILDINFO omits it.
pub fn resolve_bundle_version() -> Result<String> {
    if let Ok(forced) = std::env::var("FMF_BUILD_VERSION") {
        let forced = forced.trim();
        if !forced.is_empty() {
            return Ok(forced.to_owned());
        }
    }
    let base = workspace_base_version()?;
    let full = compute(&base, Channel::Dev, None, git_short_sha().as_deref())?;
    Ok(append_dirty(&full, git_tree_is_dirty()))
}

/// Append the `.dirty` build-metadata marker when the working tree carries
/// uncommitted changes — but only next to a real sha (`full` already ends in the
/// `+g<sha>` metadata a dirty marker attaches to). A version with no git metadata
/// is left untouched, mirroring the `Some(sha)`-only placement in the
/// fmf-buildstamp / fmf-launcher build.rs, so the marker never lands on a bare
/// `…-dev`. Pure: the caller supplies the dirtiness so this stays git/FS-free.
fn append_dirty(full: &str, dirty: bool) -> String {
    if dirty && full.contains("+g") {
        format!("{full}.dirty")
    } else {
        full.to_owned()
    }
}

/// `git status --porcelain` prints one line per uncommitted change; a non-empty
/// result means the tree is dirty. Best-effort (git absent → treated as clean).
fn git_tree_is_dirty() -> bool {
    cmd::capture(&paths::repo_root(), "git", &["status", "--porcelain"])
        .is_some_and(|s| !s.is_empty())
}

/// `git show -s --format=%cs HEAD` — the HEAD commit date (`YYYY-MM-DD`). Used for
/// the `date:` field on dev/stable bundles (reproducible; no wall clock).
pub fn git_commit_date() -> Option<String> {
    cmd::capture(
        &paths::repo_root(),
        "git",
        &["show", "-s", "--format=%cs", "HEAD"],
    )
    .filter(|s| !s.is_empty())
}

fn workspace_base_version() -> Result<String> {
    let path = paths::engine_cargo_toml();
    let src = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let doc: DocumentMut = src.parse().context("parse engine/Cargo.toml")?;
    let version = doc
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(toml_edit::Item::as_str)
        .context("engine/Cargo.toml has no [workspace.package] version")?;
    Ok(version.to_owned())
}

fn git_short_sha() -> Option<String> {
    cmd::capture(
        &paths::repo_root(),
        "git",
        &["rev-parse", "--short=7", "HEAD"],
    )
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_matches_accepts_equal_versions() {
        assert!(tag_matches("v0.1.0", "0.1.0").is_ok());
        assert!(tag_matches("V0.1.0", "0.1.0").is_ok());
        assert!(tag_matches("0.1.0", "0.1.0").is_ok());
    }

    #[test]
    fn tag_matches_rejects_a_drifted_tag() {
        assert!(tag_matches("v0.2.0", "0.1.0").is_err());
        assert!(tag_matches("v0.1.1", "0.1.0").is_err());
        assert!(tag_matches("v1.0.0", "0.1.0").is_err());
    }

    #[test]
    fn stable_is_the_clean_base() {
        assert_eq!(
            compute("0.1.0", Channel::Stable, None, Some("abc1234")).unwrap(),
            "0.1.0"
        );
    }

    #[test]
    fn dev_carries_channel_and_sha() {
        assert_eq!(
            compute("0.1.0", Channel::Dev, None, Some("abc1234")).unwrap(),
            "0.1.0-dev+gabc1234"
        );
    }

    #[test]
    fn dev_without_sha_drops_metadata() {
        assert_eq!(
            compute("0.1.0", Channel::Dev, None, None).unwrap(),
            "0.1.0-dev"
        );
    }

    #[test]
    fn nightly_embeds_date_and_sha() {
        assert_eq!(
            compute("0.1.0", Channel::Nightly, Some("20260629"), Some("abc1234")).unwrap(),
            "0.1.0-nightly.20260629+gabc1234"
        );
    }

    #[test]
    fn nightly_requires_a_date() {
        assert!(compute("0.1.0", Channel::Nightly, None, Some("abc1234")).is_err());
    }

    #[test]
    fn nightly_rejects_a_malformed_date() {
        for bad in ["2026-06-29", "20260", "2026062x", ""] {
            assert!(
                compute("0.1.0", Channel::Nightly, Some(bad), None).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn channel_parse_rejects_unknown() {
        assert!(Channel::parse("canary").is_err());
        assert_eq!(Channel::parse("nightly").unwrap(), Channel::Nightly);
    }

    #[test]
    fn identity_classifies_stable() {
        let id = parse_identity("0.1.0");
        assert_eq!(id.channel, "stable");
        assert_eq!(id.commit, None);
        assert_eq!(id.date, None);
    }

    #[test]
    fn identity_classifies_dev_with_sha() {
        let id = parse_identity("0.1.0-dev+gabc1234");
        assert_eq!(id.channel, "dev");
        assert_eq!(id.commit.as_deref(), Some("abc1234"));
        assert_eq!(id.date, None);
    }

    #[test]
    fn identity_keeps_dirty_suffix_on_commit() {
        let id = parse_identity("0.1.0-dev+gabc1234.dirty");
        assert_eq!(id.channel, "dev");
        assert_eq!(id.commit.as_deref(), Some("abc1234.dirty"));
    }

    #[test]
    fn append_dirty_marks_only_next_to_a_sha() {
        // Dirty dev build with a sha → the agreed `.dirty` marker, byte-identical
        // to what fmf-buildstamp / fmf-launcher / the C# target now stamp.
        assert_eq!(
            append_dirty("0.1.0-dev+gabc1234", true),
            "0.1.0-dev+gabc1234.dirty"
        );
        // Clean tree → untouched.
        assert_eq!(
            append_dirty("0.1.0-dev+gabc1234", false),
            "0.1.0-dev+gabc1234"
        );
        // No git metadata → never `…-dev.dirty`, even when dirty (mirrors the
        // `Some(sha)`-only append in the build.rs stampers).
        assert_eq!(append_dirty("0.1.0-dev", true), "0.1.0-dev");
    }

    #[test]
    fn dirty_dev_version_round_trips_through_parse_identity() {
        // The full contract: the string the four stampers agree on must decode
        // back to a dev build whose commit carries the `.dirty` marker.
        let full = append_dirty("0.1.0-dev+gabc1234", true);
        let id = parse_identity(&full);
        assert_eq!(id.channel, "dev");
        assert_eq!(id.commit.as_deref(), Some("abc1234.dirty"));
        assert_eq!(id.date, None);
    }

    #[test]
    fn buildinfo_dirty_dev_carries_dirty_commit_line() {
        // A dirty local `just publish` stamps BUILDINFO's commit line with the same
        // `.dirty` marker the exes report — the version never disagrees.
        let full = append_dirty("0.1.0-dev+gabc1234", true);
        let body = render_buildinfo(&full, Some("2026-07-11"));
        assert!(body.contains("version:  0.1.0-dev+gabc1234.dirty\n"));
        assert!(body.contains("channel:  dev\n"));
        assert!(body.contains("commit:   abc1234.dirty\n"));
    }

    #[test]
    fn identity_classifies_nightly_with_date_and_sha() {
        let id = parse_identity("0.1.0-nightly.20260629+gabc1234");
        assert_eq!(id.channel, "nightly");
        assert_eq!(id.date.as_deref(), Some("20260629"));
        assert_eq!(id.commit.as_deref(), Some("abc1234"));
    }

    #[test]
    fn identity_dev_without_metadata() {
        let id = parse_identity("0.1.0-dev");
        assert_eq!(id.channel, "dev");
        assert_eq!(id.commit, None);
    }

    #[test]
    fn buildinfo_nightly_reformats_embedded_date_over_commit_date() {
        let body = render_buildinfo("0.1.0-nightly.20260629+gabc1234", Some("2026-06-15"));
        assert!(body.starts_with("FindMyFiles\n"));
        assert!(body.contains("version:  0.1.0-nightly.20260629+gabc1234\n"));
        assert!(body.contains("channel:  nightly\n"));
        assert!(body.contains("commit:   abc1234\n"));
        // Nightly's own build date wins over the commit date.
        assert!(body.contains("date:     2026-06-29\n"));
        assert!(body.contains("license:  Apache-2.0\n"));
    }

    #[test]
    fn buildinfo_dev_uses_commit_date_and_omits_absent_fields() {
        let body = render_buildinfo("0.1.0-dev", None);
        assert!(body.contains("channel:  dev\n"));
        // No sha, no date known → those lines are omitted, not blank.
        assert!(!body.contains("commit:"));
        assert!(!body.contains("date:"));
    }

    #[test]
    fn buildinfo_stable_is_clean() {
        let body = render_buildinfo("0.1.0", Some("2026-06-29"));
        assert!(body.contains("version:  0.1.0\n"));
        assert!(body.contains("channel:  stable\n"));
        assert!(body.contains("date:     2026-06-29\n"));
        assert!(!body.contains("commit:"));
    }
}
