//! `xtask version --channel <dev|nightly|stable> [--date YYYYMMDD]` — print the
//! canonical channel-aware version string. This is the single source of the
//! *format*: CI exports the result as `FMF_BUILD_VERSION` so the fmf-buildstamp
//! build.rs stamps it verbatim. `xtask publish` records that same identity in
//! `BUILDINFO.txt`; packaging then names the zip from the assembled artifact.
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
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
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

/// `xtask check-version <tag>`: hard-fail unless every release version authority
/// contains the same canonical `X.Y.Z`: the tag, release-please manifest, Rust
/// workspace, C# project and every local engine package in `Cargo.lock`.
///
/// release.yml runs this BEFORE signing/packaging, so a partially-applied
/// release-please update or stale lockfile cannot ship mislabeled artifacts.
pub fn check_release_tag(tag: &str) -> Result<()> {
    let root = paths::repo_root();
    let engine = paths::engine_dir();

    let engine_toml_path = paths::engine_cargo_toml();
    let engine_toml = read_text(&engine_toml_path)?;
    let workspace = parse_workspace_version(&engine_toml)
        .with_context(|| format!("read version from {}", engine_toml_path.display()))?;
    let workspace_members = workspace_member_names(&engine, &engine_toml)?;

    let manifest_path = root.join(".release-please-manifest.json");
    let manifest = parse_release_please_version(&read_text(&manifest_path)?)
        .with_context(|| format!("read version from {}", manifest_path.display()))?;

    let csproj_path = root.join("app/FindMyFiles/FindMyFiles.csproj");
    let csproj = parse_csproj_version(&read_text(&csproj_path)?)
        .with_context(|| format!("read version from {}", csproj_path.display()))?;

    let lock_path = engine.join("Cargo.lock");
    let lock_versions = parse_local_lock_versions(&read_text(&lock_path)?, &workspace_members)
        .with_context(|| format!("read local workspace versions from {}", lock_path.display()))?;

    validate_release_versions(tag, &manifest, &workspace, &csproj, &lock_versions)
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

/// Pure comparison behind [`check_release_tag`] — unit-tested without the FS.
#[cfg(test)]
fn tag_matches(tag: &str, committed: &str) -> Result<()> {
    let want = semver::strip_tag_v(tag);
    semver::validate(want).with_context(|| format!("release tag '{tag}' is not canonical"))?;
    semver::validate(committed).context("committed workspace version is not canonical")?;
    if want != committed {
        bail!(
            "release tag '{tag}' (version {want}) does not match the committed \
             workspace version {committed} in engine/Cargo.toml — bump one so they agree"
        );
    }
    Ok(())
}

/// Validate all release sources at once and report the precise drifted source.
fn validate_release_versions(
    tag: &str,
    manifest: &str,
    workspace: &str,
    csproj: &str,
    lock_versions: &BTreeMap<String, String>,
) -> Result<()> {
    let tag_version = semver::strip_tag_v(tag);
    let authorities = [
        ("release tag", tag_version),
        (".release-please-manifest.json[\".\"]", manifest),
        ("engine/Cargo.toml workspace.package.version", workspace),
        ("app/FindMyFiles/FindMyFiles.csproj Version", csproj),
    ];
    for (source, version) in authorities {
        semver::validate(version)
            .with_context(|| format!("{source} has non-canonical version '{version}'"))?;
    }
    if lock_versions.is_empty() {
        bail!("engine/Cargo.lock contains no local workspace packages");
    }
    for (package, version) in lock_versions {
        semver::validate(version).with_context(|| {
            format!("engine/Cargo.lock package '{package}' has non-canonical version '{version}'")
        })?;
    }

    let expected = tag_version;
    for (source, actual) in authorities.into_iter().skip(1) {
        if actual != expected {
            bail!(
                "release version drift: {source} is '{actual}', but release tag \
                 '{tag}' requires '{expected}'"
            );
        }
    }
    for (package, actual) in lock_versions {
        if actual != expected {
            bail!(
                "release version drift: engine/Cargo.lock package '{package}' is \
                 '{actual}', but release tag '{tag}' requires '{expected}'"
            );
        }
    }
    Ok(())
}

fn parse_release_please_version(source: &str) -> Result<String> {
    let document: Value =
        serde_json::from_str(source).context("parse .release-please-manifest.json")?;
    document
        .get(".")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context(".release-please-manifest.json has no string version at key '.'")
}

fn parse_csproj_version(source: &str) -> Result<String> {
    let version_re =
        Regex::new(r"(?s)<Version>\s*([^<]+?)\s*</Version>").context("compile Version regex")?;
    let versions: Vec<String> = version_re
        .captures_iter(source)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().trim().to_owned()))
        .collect();
    match versions.as_slice() {
        [version] => Ok(version.clone()),
        [] => bail!("FindMyFiles.csproj has no <Version> element"),
        _ => bail!(
            "FindMyFiles.csproj has {} <Version> elements; exactly one is required",
            versions.len()
        ),
    }
}

fn parse_workspace_version(source: &str) -> Result<String> {
    let document: DocumentMut = source.parse().context("parse engine/Cargo.toml")?;
    document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml_edit::Item::as_str)
        .map(str::to_owned)
        .context("engine/Cargo.toml has no [workspace.package] version")
}

/// Resolve the actual workspace package names from `workspace.members`; this
/// avoids a second hand-maintained crate-name list that could omit a new member.
fn workspace_member_names(engine_dir: &Path, workspace_source: &str) -> Result<Vec<String>> {
    let document: DocumentMut = workspace_source
        .parse()
        .context("parse engine/Cargo.toml for workspace members")?;
    let members = document
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml_edit::Item::as_array)
        .context("engine/Cargo.toml has no workspace.members array")?;
    let mut names = BTreeSet::new();
    for member in members {
        let rel = member
            .as_str()
            .context("engine/Cargo.toml workspace member is not a string")?;
        let manifest_path = engine_dir.join(rel).join("Cargo.toml");
        let manifest_source = read_text(&manifest_path)?;
        let manifest: DocumentMut = manifest_source
            .parse()
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        let name = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml_edit::Item::as_str)
            .with_context(|| format!("{} has no package.name", manifest_path.display()))?;
        if !names.insert(name.to_owned()) {
            bail!("duplicate workspace package name '{name}'");
        }
    }
    if names.is_empty() {
        bail!("engine/Cargo.toml workspace.members is empty");
    }
    Ok(names.into_iter().collect())
}

/// Read the lockfile entries for exactly the local workspace member names.
/// `source` must be absent: accepting a registry package with a colliding name
/// would let a missing local lock entry pass accidentally.
fn parse_local_lock_versions(
    source: &str,
    workspace_members: &[String],
) -> Result<BTreeMap<String, String>> {
    let document: DocumentMut = source.parse().context("parse engine/Cargo.lock")?;
    let packages = document
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .context("engine/Cargo.lock has no [[package]] entries")?;
    let mut versions = BTreeMap::new();
    for member in workspace_members {
        let matches: Vec<_> = packages
            .iter()
            .filter(|package| {
                package.get("name").and_then(toml_edit::Item::as_str) == Some(member.as_str())
                    && package.get("source").is_none()
            })
            .collect();
        let [package] = matches.as_slice() else {
            bail!(
                "engine/Cargo.lock must contain exactly one source-less entry for \
                 workspace package '{member}', found {}",
                matches.len()
            );
        };
        let version = package
            .get("version")
            .and_then(toml_edit::Item::as_str)
            .with_context(|| format!("engine/Cargo.lock package '{member}' has no version"))?;
        if versions
            .insert(member.clone(), version.to_owned())
            .is_some()
        {
            bail!("duplicate local lockfile version collected for '{member}'");
        }
    }
    Ok(versions)
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

/// Require the immutable source identity used by release artifacts. This is
/// deliberately stricter than the seven-character display SHA carried in local
/// dev/nightly versions: a release source identity is exactly one lowercase
/// forty-hex Git object name.
pub fn validate_source_commit(commit: &str) -> Result<()> {
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("source commit must be exactly 40 lowercase hexadecimal characters, got '{commit}'");
    }
    Ok(())
}

/// Resolve the immutable source identity independently of the human-facing
/// build version. CI may pin it explicitly; local builds derive the full HEAD
/// object name while keeping their existing short/dirty version display. A
/// present empty, malformed, or non-Unicode override is untrusted release input
/// and fails closed. A source tree without Git metadata may still be assembled,
/// but its later bundle-seal boundary will reject the missing canonical commit.
pub fn resolve_source_commit() -> Result<Option<String>> {
    if let Some(raw) = std::env::var_os("FMF_SOURCE_COMMIT") {
        let commit = raw
            .into_string()
            .map_err(|_| anyhow::anyhow!("FMF_SOURCE_COMMIT must be valid Unicode"))?;
        return select_source_commit(Some(&commit), None);
    }
    let head = git_full_sha();
    select_source_commit(None, head.as_deref())
}

/// Pure precedence/validation core for [`resolve_source_commit`].
fn select_source_commit(forced: Option<&str>, git_head: Option<&str>) -> Result<Option<String>> {
    let Some((commit, source)) = forced
        .map(|commit| (commit, "FMF_SOURCE_COMMIT"))
        .or_else(|| git_head.map(|commit| (commit, "git rev-parse HEAD")))
    else {
        return Ok(None);
    };
    validate_source_commit(commit).with_context(|| format!("invalid {source}"))?;
    Ok(Some(commit.to_owned()))
}

/// Extract and validate the one canonical release source identity from
/// `BUILDINFO.txt`. Leading BOM/whitespace is accepted for the human-readable
/// file format, but empty, malformed, uppercase, and duplicate fields are not.
pub fn parse_buildinfo_source_commit(source: &str) -> Result<String> {
    let commits: Vec<&str> = source
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .filter_map(|line| line.strip_prefix("commit:").map(str::trim))
        .collect();
    let [commit] = commits.as_slice() else {
        bail!(
            "BUILDINFO.txt must contain exactly one `commit:` field (found {})",
            commits.len()
        );
    };
    validate_source_commit(commit).context("invalid BUILDINFO.txt `commit:` field")?;
    Ok((*commit).to_owned())
}

/// Render the human-and-grep friendly `BUILDINFO.txt` body (LF; the caller adds
/// the BOM/CRLF for Notepad). Pure: `full` is the stamped version,
/// `commit_date` the `git show -s --format=%cs` date (`YYYY-MM-DD`), if known,
/// and `source_commit` the optional exact release source identity. For local
/// builds without that override, the display SHA decoded from `full` is retained
/// unchanged. For nightly the date embedded in the version wins over the commit
/// date.
pub fn render_buildinfo(
    full: &str,
    commit_date: Option<&str>,
    source_commit: Option<&str>,
) -> Result<String> {
    const SOURCE: &str = "https://github.com/P4suta/find-my-files";
    let id = parse_identity(full);
    if let Some(commit) = source_commit {
        validate_source_commit(commit).context("invalid BUILDINFO source commit")?;
    }
    let commit = source_commit.or(id.commit.as_deref());
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
    if let Some(c) = commit {
        let _ = writeln!(out, "commit:   {c}");
    }
    if let Some(d) = &date {
        let _ = writeln!(out, "date:     {d}");
    }
    let _ = writeln!(out, "source:   {SOURCE}");
    out.push_str("license:  Apache-2.0\n");
    Ok(out)
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
    parse_workspace_version(&src)
}

fn git_short_sha() -> Option<String> {
    cmd::capture(
        &paths::repo_root(),
        "git",
        &["rev-parse", "--short=7", "HEAD"],
    )
    .filter(|s| !s.is_empty())
}

fn git_full_sha() -> Option<String> {
    cmd::capture(
        &paths::repo_root(),
        "git",
        &["rev-parse", "--verify", "HEAD"],
    )
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_versions(version: &str) -> BTreeMap<String, String> {
        [
            ("fmf-core".to_owned(), version.to_owned()),
            ("fmf-service".to_owned(), version.to_owned()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn all_release_authorities_must_agree() {
        let versions = lock_versions("0.1.1");
        validate_release_versions("v0.1.1", "0.1.1", "0.1.1", "0.1.1", &versions).unwrap();

        assert!(validate_release_versions("v0.1.1", "0.1.0", "0.1.1", "0.1.1", &versions).is_err());
        assert!(validate_release_versions("v0.1.1", "0.1.1", "0.1.0", "0.1.1", &versions).is_err());
        assert!(validate_release_versions("v0.1.1", "0.1.1", "0.1.1", "0.1.0", &versions).is_err());
        let drifted_lock = lock_versions("0.1.0");
        assert!(
            validate_release_versions("v0.1.1", "0.1.1", "0.1.1", "0.1.1", &drifted_lock).is_err()
        );
    }

    #[test]
    fn every_release_authority_must_be_canonical_semver() {
        for malformed in ["01.2.3", "1.02.3", "1.2.03", "1.2.3-rc.1", "1.2"] {
            let versions = lock_versions("1.2.3");
            assert!(validate_release_versions(
                &format!("v{malformed}"),
                "1.2.3",
                "1.2.3",
                "1.2.3",
                &versions
            )
            .is_err());
            assert!(
                validate_release_versions("v1.2.3", malformed, "1.2.3", "1.2.3", &versions)
                    .is_err()
            );
            let malformed_lock = lock_versions(malformed);
            assert!(validate_release_versions(
                "v1.2.3",
                "1.2.3",
                "1.2.3",
                "1.2.3",
                &malformed_lock
            )
            .is_err());
        }
    }

    #[test]
    fn release_source_parsers_fail_closed() {
        assert_eq!(
            parse_release_please_version(r#"{".":"0.1.1"}"#).unwrap(),
            "0.1.1"
        );
        assert!(parse_release_please_version(r#"{"other":"0.1.1"}"#).is_err());
        assert!(parse_release_please_version(r#"{".":1}"#).is_err());

        assert_eq!(
            parse_csproj_version("<Project><Version> 0.1.1 </Version></Project>").unwrap(),
            "0.1.1"
        );
        assert!(parse_csproj_version("<Project />").is_err());
        assert!(parse_csproj_version(
            "<Project><Version>0.1.1</Version><Version>0.1.2</Version></Project>"
        )
        .is_err());

        assert_eq!(
            parse_workspace_version("[workspace.package]\nversion = \"0.1.1\"\n").unwrap(),
            "0.1.1"
        );
        assert!(parse_workspace_version("[workspace]\nmembers = []\n").is_err());
    }

    #[test]
    fn lock_parser_requires_one_source_less_entry_per_workspace_member() {
        let members = vec!["fmf-core".to_owned(), "fmf-service".to_owned()];
        let lock = "\
version = 4

[[package]]
name = \"fmf-core\"
version = \"0.1.1\"

[[package]]
name = \"fmf-service\"
version = \"0.1.1\"
";
        let parsed = parse_local_lock_versions(lock, &members).unwrap();
        assert_eq!(parsed["fmf-core"], "0.1.1");
        assert_eq!(parsed["fmf-service"], "0.1.1");

        assert!(parse_local_lock_versions(
            "[[package]]\nname = \"fmf-core\"\nversion = \"0.1.1\"\n",
            &members
        )
        .is_err());
        assert!(parse_local_lock_versions(
            "[[package]]\nname = \"fmf-core\"\nversion = \"0.1.1\"\n\
             source = \"registry+https://example.invalid/index\"\n\
             [[package]]\nname = \"fmf-service\"\nversion = \"0.1.1\"\n",
            &members
        )
        .is_err());
    }

    #[test]
    fn committed_release_sources_and_lockfile_are_in_lockstep() {
        let manifest =
            fs::read_to_string(paths::repo_root().join(".release-please-manifest.json")).unwrap();
        let version = parse_release_please_version(&manifest).unwrap();
        check_release_tag(&format!("v{version}")).unwrap();
    }

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
        let body = render_buildinfo(&full, Some("2026-07-11"), None).unwrap();
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
        let body =
            render_buildinfo("0.1.0-nightly.20260629+gabc1234", Some("2026-06-15"), None).unwrap();
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
        let body = render_buildinfo("0.1.0-dev", None, None).unwrap();
        assert!(body.contains("channel:  dev\n"));
        // No sha, no date known → those lines are omitted, not blank.
        assert!(!body.contains("commit:"));
        assert!(!body.contains("date:"));
    }

    #[test]
    fn buildinfo_stable_is_clean() {
        let body = render_buildinfo("0.1.0", Some("2026-06-29"), None).unwrap();
        assert!(body.contains("version:  0.1.0\n"));
        assert!(body.contains("channel:  stable\n"));
        assert!(body.contains("date:     2026-06-29\n"));
        assert!(!body.contains("commit:"));
    }

    #[test]
    fn buildinfo_stable_pins_the_exact_release_source_commit() {
        let source_commit = "0123456789abcdef0123456789abcdef01234567";
        let body = render_buildinfo("0.1.0", Some("2026-06-29"), Some(source_commit)).unwrap();

        assert!(body.contains("version:  0.1.0\n"));
        assert!(body.contains("channel:  stable\n"));
        assert!(body.contains(&format!("commit:   {source_commit}\n")));
        assert_eq!(parse_buildinfo_source_commit(&body).unwrap(), source_commit);
    }

    #[test]
    fn local_dev_buildinfo_uses_full_head_without_changing_short_dirty_version() {
        let source_commit = "0123456789abcdef0123456789abcdef01234567";
        let displayed = append_dirty("0.1.0-dev+g0123456", true);
        let body = render_buildinfo(&displayed, Some("2026-06-29"), Some(source_commit)).unwrap();

        assert!(body.contains("version:  0.1.0-dev+g0123456.dirty\n"));
        assert!(body.contains(&format!("commit:   {source_commit}\n")));
    }

    #[test]
    fn source_commit_prefers_explicit_ci_identity_then_full_git_head() {
        let forced = "0123456789abcdef0123456789abcdef01234567";
        let head = "89abcdef0123456789abcdef0123456789abcdef";

        assert_eq!(
            select_source_commit(Some(forced), Some(head)).unwrap(),
            Some(forced.to_owned())
        );
        assert_eq!(
            select_source_commit(None, Some(head)).unwrap(),
            Some(head.to_owned())
        );
        assert_eq!(select_source_commit(None, None).unwrap(), None);
        assert!(select_source_commit(Some(""), Some(head)).is_err());
        assert!(
            select_source_commit(None, Some("89ABCDEF0123456789abcdef0123456789abcdef")).is_err()
        );
    }

    #[test]
    fn source_commit_rejects_empty_short_uppercase_and_non_hex_values() {
        for invalid in [
            "",
            "0123456",
            "0123456789abcdef0123456789abcdef0123456A",
            "0123456789abcdef0123456789abcdef0123456g",
        ] {
            assert!(validate_source_commit(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn buildinfo_source_commit_requires_exactly_one_canonical_field() {
        let valid = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_buildinfo_source_commit(&format!("\u{feff}FindMyFiles\r\ncommit:   {valid}\r\n"))
                .unwrap(),
            valid
        );

        for invalid in [
            "FindMyFiles\n",
            "FindMyFiles\ncommit:   \n",
            "FindMyFiles\ncommit:   0123456789abcdef0123456789abcdef0123456A\n",
            "FindMyFiles\ncommit:   0123456789abcdef0123456789abcdef01234567\ncommit:   89abcdef0123456789abcdef0123456789abcdef\n",
        ] {
            assert!(parse_buildinfo_source_commit(invalid).is_err());
        }
    }
}
