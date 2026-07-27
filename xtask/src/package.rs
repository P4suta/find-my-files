//! `xtask package [tag]` — zip + checksum the assembled bundle.
//!
//! Replaces release.yml's `Compress-Archive` + `Get-FileHash` steps. Runs
//! AFTER the signing step (which signs the PE files in dist/), so the zip
//! contains the signed binaries. Both land in build/package/ (ADR-0021) —
//! release.yml's `action-gh-release` glob points there:
//!   find-my-files-<bundle-version>-win-x64.zip
//!   SHA256SUMS.txt                          (coreutils `sha256sum -c` format)
//!
//! The assembled bundle's `BUILDINFO.txt` is the artifact-identity source of
//! truth. A stable tag must match it exactly; tagless dev/nightly packaging uses
//! it verbatim. This prevents an environment variable from naming a zip
//! differently from the binaries and BUILDINFO carried inside it.

use crate::{
    bundle_seal::{self, BundleFileIdentity, BundleState},
    checksum, fsx, paths, pe_load, publish, semver,
};
use anyhow::{bail, ensure, Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub fn run(tag: Option<&str>) -> Result<()> {
    const SUMS_NAME: &str = "SHA256SUMS.txt";
    let dist = paths::dist_dir();
    if !dist.exists() {
        bail!(
            "{} does not exist — run `just publish` first",
            dist.display()
        );
    }
    let bundle_state = if tag.is_some() {
        BundleState::Signed
    } else {
        BundleState::Unsigned
    };
    let sealed_files = bundle_seal::collect_bundle_files(&dist, bundle_state)
        .context("package refuses a non-exact or wrongly signed distribution tree")?;
    verify_release_boundary(&dist)?;
    let bundle_version = read_bundle_version(&dist)?;
    verify_shipping_readme(&dist)?;
    let label = package_label(tag, &bundle_version)?;

    let pkg = paths::package_dir();
    prepare_package_dir(&pkg)?;

    let zip_name = format!("find-my-files-{label}-win-x64.zip");
    let zip_path = pkg.join(&zip_name);
    write_zip(&dist, &zip_path, &sealed_files)?;

    // SHA256SUMS lists every distributable in build/package (currently the zip;
    // directory-driven so any future artifact dropped here is covered too) in
    // coreutils format, verifiable with `sha256sum -c SHA256SUMS.txt`.
    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(&pkg).with_context(|| format!("read {}", pkg.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == SUMS_NAME {
            continue; // never checksum the sums file itself
        }
        let bytes =
            fs::read(entry.path()).with_context(|| format!("read {}", entry.path().display()))?;
        entries.push((checksum::sha256_hex(&bytes), name));
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1)); // deterministic line order

    let sums_path = pkg.join(SUMS_NAME);
    fs::write(&sums_path, checksum::sha256sums_body(&entries))
        .with_context(|| format!("write {}", sums_path.display()))?;

    println!("packaged into {}:", pkg.display());
    for (hash, name) in &entries {
        println!("{hash}  {name}");
    }
    Ok(())
}

/// Recheck security- and surface-critical publish invariants immediately before
/// sealing the ZIP. Packaging is a separately invocable command, so it must not
/// trust that the directory was produced by this revision of `xtask publish`.
fn verify_release_boundary(dist: &Path) -> Result<()> {
    let app = dist.join("app");
    pe_load::require_system32_only(&app.join("fmf-service.exe"))
        .context("package refuses a service without System32-only static dependency loading")?;
    publish::verify_test_artifacts(&app, false)
        .context("package refuses shipping test seams or fixtures")?;
    verify_no_developer_payload(dist)
}

fn verify_no_developer_payload(dist: &Path) -> Result<()> {
    for relative in ["app/fmf.exe", "completions"] {
        let path = dist.join(relative);
        if path.exists() {
            bail!(
                "package refuses developer-only payload in the end-user bundle: {}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Read the exact identity shipped in the assembled bundle. `publish` writes
/// UTF-8 with a BOM and CRLF for Notepad, while [`str::lines`] intentionally
/// accepts both CRLF and LF so the parser remains easy to unit-test.
fn read_bundle_version(dist: &Path) -> Result<String> {
    let path = dist.join("BUILDINFO.txt");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read {} — run `just publish` first", path.display()))?;
    parse_buildinfo_version(&text)
        .with_context(|| format!("parse artifact identity from {}", path.display()))
}

/// Fail closed if packaging is pointed at a stale bundle carrying the old
/// adjacent-data/portable README. Publish owns generation; package independently
/// verifies the user-facing uninstall truth before sealing the zip.
fn verify_shipping_readme(dist: &Path) -> Result<()> {
    let path = dist.join("README.txt");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read {} — run `just publish` first", path.display()))?;
    for required in [
        "%ProgramData%\\find-my-files\\",
        "%APPDATA%\\find-my-files\\",
        "Remove service and all data...",
        "scheduled task",
        "both data directories",
    ] {
        if !text.contains(required) {
            bail!(
                "{} is stale or incomplete: missing required uninstall detail {required:?}",
                path.display()
            );
        }
    }

    let lower = text.to_ascii_lowercase();
    for forbidden in [
        "data\\  next to this file",
        "folder is portable",
        "delete it, freely",
        "ポータブル構成",
        "削除も自由",
    ] {
        if lower.contains(&forbidden.to_ascii_lowercase()) {
            bail!(
                "{} contains a false adjacent-data/portable claim {forbidden:?}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Extract one unambiguous `version:` field from BUILDINFO.
fn parse_buildinfo_version(text: &str) -> Result<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let versions: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("version:").map(str::trim))
        .collect();
    let [version] = versions.as_slice() else {
        bail!(
            "BUILDINFO must contain exactly one `version:` field (found {})",
            versions.len()
        );
    };
    validate_artifact_version(version)?;
    Ok((*version).to_owned())
}

/// Turn the bundle identity into the zip label. Stable releases keep their
/// conventional `v` prefix, but only after proving that the requested release
/// tag and the already-built payload are byte-for-byte the same version.
fn package_label(tag: Option<&str>, bundle_version: &str) -> Result<String> {
    validate_artifact_version(bundle_version)?;
    if let Some(tag) = tag {
        let tagged_version = semver::strip_tag_v(tag);
        semver::validate(tagged_version)?;
        if tagged_version != bundle_version {
            bail!(
                "release tag '{tag}' names version {tagged_version}, but the assembled \
                 bundle is {bundle_version} — rebuild with the stable stamp before packaging"
            );
        }
        Ok(format!("v{tagged_version}"))
    } else {
        Ok(bundle_version.to_owned())
    }
}

/// BUILDINFO is allowed to carry dev/nightly `SemVer` suffixes, so the strict
/// release-triple validator is intentionally not used here. This gate is about
/// an untrusted text field becoming a Windows filename: require a compact,
/// path-free, SemVer-shaped label and fail closed on whitespace/control chars.
fn validate_artifact_version(version: &str) -> Result<()> {
    let bytes = version.as_bytes();
    let safe = !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_digit()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    if !safe {
        bail!(
            "BUILDINFO version '{version}' is not a safe artifact label \
             (expected at most 128 ASCII letters/digits plus '.', '-' or '+', starting \
             with a digit and ending with a letter or digit)"
        );
    }
    Ok(())
}

/// Start from an empty package dir. The SHA256SUMS.txt body is driven by whatever
/// files sit here; a stale zip from an earlier version would otherwise linger in
/// the checksum set even though workflows upload the exact versioned zip path.
/// CI runs on a fresh runner where the wipe is a no-op; locally it closes the
/// footgun.
fn prepare_package_dir(pkg: &Path) -> Result<()> {
    fsx::force_remove_dir_all(pkg).with_context(|| format!("clean {}", pkg.display()))?;
    fs::create_dir_all(pkg).with_context(|| format!("create {}", pkg.display()))?;
    Ok(())
}

/// Zip the *contents* of `dist` (entries land at the zip root, matching
/// `Compress-Archive -Path dist/FindMyFiles/*`).
fn write_zip(dist: &Path, zip_path: &Path, sealed_files: &[BundleFileIdentity]) -> Result<()> {
    let file = File::create(zip_path).with_context(|| format!("create {}", zip_path.display()))?;
    let mut zw = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for identity in sealed_files {
        let absolute = dist.join(&identity.path);
        let data = fs::read(&absolute).with_context(|| format!("read {}", absolute.display()))?;
        ensure!(
            data.len() as u64 == identity.size && checksum::sha256_hex(&data) == identity.sha256,
            "sealed bundle file changed while packaging: {}",
            identity.path
        );
        zw.start_file(identity.path.as_str(), opts)
            .with_context(|| format!("zip entry {}", identity.path))?;
        zw.write_all(&data)
            .with_context(|| format!("write zip entry {}", identity.path))?;
    }
    zw.finish().context("finalize zip")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xtask-package-{tag}-{}", std::process::id()))
    }

    /// A stale zip from an earlier version must not survive into the next
    /// packaging run (else it lands in SHA256SUMS.txt and the release glob).
    #[test]
    fn prepare_package_dir_clears_stale_artifacts() {
        let pkg = scratch("prepare");
        let _ = fsx::force_remove_dir_all(&pkg);
        fs::create_dir_all(&pkg).unwrap();
        let stale = pkg.join("find-my-files-v0.0.1-win-x64.zip");
        fs::write(&stale, b"old").unwrap();

        prepare_package_dir(&pkg).unwrap();

        assert!(pkg.is_dir(), "package dir should exist after prepare");
        assert!(!stale.exists(), "stale artifact should be gone");
        assert_eq!(
            fs::read_dir(&pkg).unwrap().count(),
            0,
            "package dir should be empty"
        );

        fsx::force_remove_dir_all(&pkg).unwrap();
    }

    /// Preparing a not-yet-existing package dir just creates it (the fresh-runner
    /// path) — a missing dir is not an error.
    #[test]
    fn prepare_package_dir_creates_when_absent() {
        let pkg = scratch("absent");
        let _ = fsx::force_remove_dir_all(&pkg);

        prepare_package_dir(&pkg).unwrap();

        assert!(pkg.is_dir(), "package dir should be created");

        fsx::force_remove_dir_all(&pkg).unwrap();
    }

    #[test]
    fn parses_notepad_encoded_buildinfo() {
        let text = "\u{feff}FindMyFiles\r\nversion:  0.1.0-dev+gabc1234.dirty\r\nchannel:  dev\r\n";
        assert_eq!(
            parse_buildinfo_version(text).unwrap(),
            "0.1.0-dev+gabc1234.dirty"
        );
    }

    #[test]
    fn package_accepts_only_the_current_shipping_readme_contract() {
        let dist = scratch("readme-current");
        let _ = fsx::force_remove_dir_all(&dist);
        fs::create_dir_all(&dist).unwrap();
        fs::write(
            dist.join("README.txt"),
            "\u{feff}%ProgramData%\\find-my-files\\\r\n\
             %APPDATA%\\find-my-files\\\r\n\
             Remove service and all data...\r\n\
             service and scheduled task; both data directories\r\n",
        )
        .unwrap();

        verify_shipping_readme(&dist).unwrap();
        fsx::force_remove_dir_all(&dist).unwrap();
    }

    #[test]
    fn package_rejects_the_old_portable_adjacent_data_story() {
        let dist = scratch("readme-stale");
        let _ = fsx::force_remove_dir_all(&dist);
        fs::create_dir_all(&dist).unwrap();
        fs::write(
            dist.join("README.txt"),
            "%ProgramData%\\find-my-files\\\n\
             %APPDATA%\\find-my-files\\\n\
             Remove service and all data...\n\
             scheduled task; both data directories\n\
             The folder is portable; delete it, freely.\n",
        )
        .unwrap();

        assert!(verify_shipping_readme(&dist).is_err());
        fsx::force_remove_dir_all(&dist).unwrap();
    }

    #[test]
    fn package_rejects_developer_cli_and_completion_payloads() {
        let dist = scratch("developer-payload");
        let _ = fsx::force_remove_dir_all(&dist);
        fs::create_dir_all(dist.join("app")).unwrap();
        verify_no_developer_payload(&dist).unwrap();

        fs::write(dist.join("app/fmf.exe"), b"developer cli").unwrap();
        assert!(verify_no_developer_payload(&dist).is_err());
        fs::remove_file(dist.join("app/fmf.exe")).unwrap();

        fs::create_dir_all(dist.join("completions")).unwrap();
        assert!(verify_no_developer_payload(&dist).is_err());
        fsx::force_remove_dir_all(&dist).unwrap();
    }

    #[test]
    fn rejects_missing_or_ambiguous_buildinfo_versions() {
        assert!(parse_buildinfo_version("FindMyFiles\nchannel: dev\n").is_err());
        assert!(
            parse_buildinfo_version("version: 0.1.0\nversion: 0.2.0\n").is_err(),
            "duplicate identity must fail closed"
        );
    }

    #[test]
    fn tagless_package_uses_the_bundle_identity() {
        assert_eq!(
            package_label(None, "0.1.0-nightly.20260725+gabc1234").unwrap(),
            "0.1.0-nightly.20260725+gabc1234"
        );
        assert_eq!(
            package_label(None, "0.1.0-dev+gabc1234.dirty").unwrap(),
            "0.1.0-dev+gabc1234.dirty"
        );
    }

    #[test]
    fn stable_tag_must_match_the_built_payload() {
        assert_eq!(package_label(Some("v1.2.3"), "1.2.3").unwrap(), "v1.2.3");
        assert!(package_label(Some("v1.2.4"), "1.2.3").is_err());
        assert!(
            package_label(Some("v1.2.3"), "1.2.3-dev+gabc1234").is_err(),
            "a dev-stamped payload must never masquerade as a stable release"
        );
    }

    #[test]
    fn unsafe_artifact_versions_cannot_become_paths() {
        for bad in [
            "",
            "../0.1.0",
            r"0.1.0\payload",
            "v0.1.0",
            "0.1.0 ",
            ".0.1.0",
            "0.1.0+",
            "0.1.0/evil",
        ] {
            assert!(
                package_label(None, bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
