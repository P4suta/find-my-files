//! `xtask release <version>` — bump the release version across the Rust
//! workspace and the C# app in lockstep, commit, and create a signed vX.Y.Z
//! tag. Pushing the tag fires release.yml.
//!
//! Guards the old PowerShell recipe lacked: semver validation, a refusal to
//! clobber an existing tag, a no-op check (version already at target), and a
//! `--dry-run` that shows the diff without committing.

use crate::{cmd, paths, semver, version};
use anyhow::{bail, Context, Result};
use std::fs;

// Repo-root-relative paths for tidy git output.
const REL_CARGO: &str = "engine/Cargo.toml";
const REL_CSPROJ: &str = "app/FindMyFiles/FindMyFiles.csproj";

pub fn run(version_arg: &str, dry_run: bool) -> Result<()> {
    semver::validate(version_arg)?;
    let root = paths::repo_root();
    let tag = format!("v{version_arg}");

    if cmd::succeeds(
        &root,
        "git",
        &["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")],
    )? {
        bail!("tag {tag} already exists — bump to a new version or delete the tag first");
    }

    let cargo_path = paths::engine_cargo_toml();
    let csproj_path = paths::app_csproj();
    let old_cargo = fs::read_to_string(&cargo_path)
        .with_context(|| format!("read {}", cargo_path.display()))?;
    let old_csproj = fs::read_to_string(&csproj_path)
        .with_context(|| format!("read {}", csproj_path.display()))?;

    let new_cargo = version::cargo_toml::set_version(&old_cargo, version_arg)?;
    let new_csproj = version::csproj::set_version(&old_csproj, version_arg)?;

    if new_cargo == old_cargo && new_csproj == old_csproj {
        bail!("version is already {version_arg} — nothing to do");
    }

    fs::write(&cargo_path, &new_cargo)
        .with_context(|| format!("write {}", cargo_path.display()))?;
    fs::write(&csproj_path, &new_csproj)
        .with_context(|| format!("write {}", csproj_path.display()))?;

    let diff = cmd::run(
        &root,
        "git",
        &["--no-pager", "diff", "--", REL_CARGO, REL_CSPROJ],
    );

    if dry_run {
        // Revert first, then surface any diff error — never leave the tree dirty.
        fs::write(&cargo_path, &old_cargo)?;
        fs::write(&csproj_path, &old_csproj)?;
        diff?;
        println!("dry-run: reverted file changes; no commit or tag created.");
        return Ok(());
    }
    diff?;

    cmd::run(&root, "git", &["add", REL_CARGO, REL_CSPROJ])?;
    cmd::run(
        &root,
        "git",
        &["commit", "-m", &format!("chore: release {tag}")],
    )?;
    cmd::run(&root, "git", &["tag", "-s", &tag, "-m", &tag])?;
    println!("Tagged {tag} — push with: git push; git push origin {tag}");
    Ok(())
}
