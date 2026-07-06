//! `xtask sign-stage` / `xtask sign-collect` — the file shuffling around the
//! release signing step, moved out of two inline-PowerShell blocks in
//! release.yml so the first-party PE map lives in exactly one place
//! ([`crate::publish::FIRST_PARTY_PES`]) instead of being duplicated per step.
//!
//! `sign-stage` copies our own PEs out of the assembled bundle into a flat dir
//! under unique names (two share the basename `FindMyFiles.exe`), which the
//! eSigner Action then batch-signs into a sibling `signed/` dir. `sign-collect`
//! copies the signed PEs back over the bundle. The signing itself (Authenticode,
//! a Windows-only API) stays in the workflow between the two.

use crate::{fsx, paths, publish::FIRST_PARTY_PES};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Populate `stage_dir` with one uniquely-named copy of each first-party PE from
/// the bundle at `dist`. Pure w.r.t. the caller's paths so it is unit-testable.
fn stage(dist: &Path, stage_dir: &Path) -> Result<()> {
    fs::create_dir_all(stage_dir)
        .with_context(|| format!("create stage dir {}", stage_dir.display()))?;
    for (src, stage_name) in FIRST_PARTY_PES {
        let from = dist.join(src);
        let to = stage_dir.join(stage_name);
        fs::copy(&from, &to)
            .with_context(|| format!("stage {} -> {}", from.display(), to.display()))?;
    }
    Ok(())
}

/// Copy each signed PE from `signed_dir` (named by its stage name) back over its
/// original path in the bundle at `dist`. The reverse of [`stage`], driven by
/// the same map.
fn collect(dist: &Path, signed_dir: &Path) -> Result<()> {
    for (src, stage_name) in FIRST_PARTY_PES {
        let from = signed_dir.join(stage_name);
        let to = dist.join(src);
        fs::copy(&from, &to)
            .with_context(|| format!("collect {} -> {}", from.display(), to.display()))?;
    }
    Ok(())
}

/// `xtask sign-stage`: stage the bundle's first-party PEs for signing and make
/// sure the eSigner Action's output dir exists (it writes the signed copies
/// there; `sign-collect` reads them back).
pub fn run_stage() -> Result<()> {
    let dist = paths::dist_dir();
    let stage_dir = paths::sign_stage_dir();
    let signed_dir = paths::signed_dir();

    // Start from a clean stage/signed pair so a re-run never mixes in stale PEs.
    fsx::force_remove_dir_all(&stage_dir)
        .with_context(|| format!("clear {}", stage_dir.display()))?;
    fsx::force_remove_dir_all(&signed_dir)
        .with_context(|| format!("clear {}", signed_dir.display()))?;

    stage(&dist, &stage_dir)?;
    fs::create_dir_all(&signed_dir)
        .with_context(|| format!("create signed dir {}", signed_dir.display()))?;

    println!(
        "sign-stage: staged {} first-party PE(s) into {}",
        FIRST_PARTY_PES.len(),
        stage_dir.display()
    );
    Ok(())
}

/// `xtask sign-collect`: copy the signed PEs back into the bundle.
pub fn run_collect() -> Result<()> {
    let dist = paths::dist_dir();
    let signed_dir = paths::signed_dir();
    collect(&dist, &signed_dir)?;
    println!(
        "sign-collect: copied {} signed PE(s) back into {}",
        FIRST_PARTY_PES.len(),
        dist.display()
    );
    Ok(())
}

/// The single packed `.msix` in `dir` — there is exactly one per build (one
/// architecture, one channel), so zero or many is a staleness/mixup error rather
/// than something to guess at.
fn find_msix(dir: &Path) -> Result<PathBuf> {
    let mut hits: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("msix") {
            hits.push(path);
        }
    }
    match hits.len() {
        1 => Ok(hits.pop().expect("len checked == 1")),
        0 => bail!(
            "no .msix in {} — run `just package-msix <tag>` first",
            dir.display()
        ),
        n => bail!(
            "{n} .msix files in {} — expected exactly one",
            dir.display()
        ),
    }
}

/// `xtask sign-stage-msix`: copy the packed `.msix` out of `build/package` into a
/// clean flat dir for the eSigner Action, and make its output dir exist. Mirrors
/// [`run_stage`] for the package wrapper (a second signing pass, since the `.msix`
/// is packed only after its payload PEs are signed).
pub fn run_stage_msix() -> Result<()> {
    let msix = find_msix(&paths::package_dir())?;
    let stage_dir = paths::msix_sign_dir();
    let signed_dir = paths::msix_signed_dir();

    fsx::force_remove_dir_all(&stage_dir)
        .with_context(|| format!("clear {}", stage_dir.display()))?;
    fsx::force_remove_dir_all(&signed_dir)
        .with_context(|| format!("clear {}", signed_dir.display()))?;
    fs::create_dir_all(&stage_dir).with_context(|| format!("create {}", stage_dir.display()))?;
    fs::create_dir_all(&signed_dir).with_context(|| format!("create {}", signed_dir.display()))?;

    let name = msix.file_name().context("the .msix has no file name")?;
    let to = stage_dir.join(name);
    fs::copy(&msix, &to)
        .with_context(|| format!("stage {} -> {}", msix.display(), to.display()))?;
    println!(
        "sign-stage-msix: staged {} into {}",
        msix.display(),
        to.display()
    );
    Ok(())
}

/// `xtask sign-collect-msix`: copy the signed `.msix` back over the one in
/// `build/package`, so the checksum sweep + release attach see the signed wrapper.
pub fn run_collect_msix() -> Result<()> {
    let signed = find_msix(&paths::msix_signed_dir())?;
    let name = signed
        .file_name()
        .context("the signed .msix has no file name")?;
    let dest = paths::package_dir().join(name);
    fs::copy(&signed, &dest)
        .with_context(|| format!("collect {} -> {}", signed.display(), dest.display()))?;
    println!(
        "sign-collect-msix: copied signed {} back into {}",
        signed.display(),
        dest.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xtask-signing-{tag}-{}", std::process::id()))
    }

    /// The whole reason the map is explicit: a flat copy-by-basename would
    /// collide (two `FindMyFiles.exe`), so every stage name must be distinct.
    #[test]
    fn stage_names_are_unique() {
        let mut seen = HashSet::new();
        for (_, stage_name) in FIRST_PARTY_PES {
            assert!(
                seen.insert(*stage_name),
                "duplicate stage name {stage_name}"
            );
        }
    }

    /// Sources, by contrast, are the real (possibly colliding) bundle paths.
    #[test]
    fn sources_are_distinct_paths() {
        let mut seen = HashSet::new();
        for (src, _) in FIRST_PARTY_PES {
            assert!(seen.insert(*src), "duplicate source path {src}");
        }
    }

    /// Fabricate a bundle, stage it, sign it (mutate the staged bytes), collect,
    /// and assert every original path now carries its signed content — the
    /// round-trip the two workflow steps perform.
    #[test]
    fn stage_sign_collect_round_trips() {
        let base = scratch("roundtrip");
        let _ = fsx::force_remove_dir_all(&base);
        let dist = base.join("dist");
        let stage_dir = base.join("sign-stage");
        let signed_dir = base.join("signed");

        // Lay down each first-party PE with unsigned content unique per file so a
        // wrong-file copy-back would be caught.
        for (src, _) in FIRST_PARTY_PES {
            let path = dist.join(src);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("unsigned:{src}")).unwrap();
        }

        stage(&dist, &stage_dir).unwrap();

        // Simulate the eSigner Action: read each staged PE, write a "signed"
        // variant into signed/ under the same stage name.
        for (_, stage_name) in FIRST_PARTY_PES {
            let staged = fs::read_to_string(stage_dir.join(stage_name)).unwrap();
            fs::create_dir_all(&signed_dir).unwrap();
            fs::write(
                signed_dir.join(stage_name),
                staged.replace("unsigned:", "signed:"),
            )
            .unwrap();
        }

        collect(&dist, &signed_dir).unwrap();

        for (src, _) in FIRST_PARTY_PES {
            assert_eq!(
                fs::read_to_string(dist.join(src)).unwrap(),
                format!("signed:{src}"),
                "{src} was not replaced with its signed copy"
            );
        }

        fsx::force_remove_dir_all(&base).unwrap();
    }

    /// `find_msix` must insist on exactly one package — zero (nothing packed) and
    /// many (a stale artifact left behind) are both errors, never a silent guess.
    #[test]
    fn find_msix_requires_exactly_one() {
        let base = scratch("findmsix");
        let _ = fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        assert!(find_msix(&base).is_err(), "zero .msix should error");

        let one = base.join("find-my-files-v1.2.3-win-x64.msix");
        fs::write(&one, b"x").unwrap();
        assert_eq!(find_msix(&base).unwrap(), one);

        fs::write(base.join("stale.msix"), b"y").unwrap();
        assert!(find_msix(&base).is_err(), "two .msix should error");

        fsx::force_remove_dir_all(&base).unwrap();
    }

    /// A missing first-party PE in the bundle is a hard error, not a silent skip
    /// — an unsigned ship is the exact failure the signing pipeline guards.
    #[test]
    fn stage_errors_when_a_pe_is_missing() {
        let base = scratch("missing");
        let _ = fsx::force_remove_dir_all(&base);
        let dist = base.join("dist");
        fs::create_dir_all(&dist).unwrap();
        // Only create the first PE; the rest are absent.
        let (first_src, _) = FIRST_PARTY_PES[0];
        let path = dist.join(first_src);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"x").unwrap();

        assert!(stage(&dist, &base.join("sign-stage")).is_err());

        fsx::force_remove_dir_all(&base).unwrap();
    }
}
