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

use crate::{cmd, paths};
use anyhow::{bail, Context, Result};
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
}
