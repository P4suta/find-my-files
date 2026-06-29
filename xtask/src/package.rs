//! `xtask package <tag>` — zip + checksum the assembled bundle for a release.
//!
//! Replaces release.yml's `Compress-Archive` + `Get-FileHash` steps. Runs
//! AFTER the signing step (which signs the PE files in dist/), so the zip
//! contains the signed binaries. Both land in build/package/ (ADR-0021) —
//! release.yml's `action-gh-release` glob points there:
//!   find-my-files-v<version>-win-x64.zip   (contents = build/dist/FindMyFiles/*)
//!   SHA256SUMS.txt                          (coreutils `sha256sum -c` format)

use crate::{checksum, fsx, paths, semver};
use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub fn run(tag: Option<&str>) -> Result<()> {
    const SUMS_NAME: &str = "SHA256SUMS.txt";
    // Stable: strict vX.Y.Z tag → `find-my-files-v0.2.0-win-x64.zip`.
    // Nightly (no tag): name from the build stamp FMF_BUILD_VERSION verbatim
    // (e.g. `find-my-files-0.1.0-nightly.20260629+g3672e3f-win-x64.zip`), which
    // already encodes the channel — no `v` prefix and no strict-semver gate.
    let label = if let Some(tag) = tag {
        let version = semver::strip_tag_v(tag);
        semver::validate(version)?;
        format!("v{version}")
    } else {
        let v = std::env::var("FMF_BUILD_VERSION").map_err(|_| {
            anyhow::anyhow!(
                "tagless (nightly) packaging needs FMF_BUILD_VERSION — set it from \
                 `xtask version --channel nightly --date YYYYMMDD`"
            )
        })?;
        let v = v.trim().to_owned();
        if v.is_empty() {
            bail!("FMF_BUILD_VERSION is set but empty");
        }
        v
    };

    let dist = paths::dist_dir();
    if !dist.exists() {
        bail!(
            "{} does not exist — run `just publish` first",
            dist.display()
        );
    }

    let pkg = paths::package_dir();
    fs::create_dir_all(&pkg).with_context(|| format!("create {}", pkg.display()))?;

    let zip_name = format!("find-my-files-{label}-win-x64.zip");
    let zip_path = pkg.join(&zip_name);
    write_zip(&dist, &zip_path)?;

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

/// Zip the *contents* of `dist` (entries land at the zip root, matching
/// `Compress-Archive -Path dist/FindMyFiles/*`).
fn write_zip(dist: &Path, zip_path: &Path) -> Result<()> {
    let file = File::create(zip_path).with_context(|| format!("create {}", zip_path.display()))?;
    let mut zw = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut files = fsx::collect_files(dist).with_context(|| format!("walk {}", dist.display()))?;
    files.sort_by(|a, b| a.1.cmp(&b.1)); // deterministic entry order

    for (abs, rel) in files {
        zw.start_file(rel.as_str(), opts)
            .with_context(|| format!("zip entry {rel}"))?;
        let data = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        zw.write_all(&data)
            .with_context(|| format!("write zip entry {rel}"))?;
    }
    zw.finish().context("finalize zip")?;
    Ok(())
}
