//! `xtask package-msix <tag>` — build the installed-experience `.msix` from the
//! already-assembled bundle (ADR-0028: packaged UI, unpackaged service).
//!
//! This runs AFTER `just publish` (so the payload PEs are the ones CI has signed)
//! and produces `build/package/find-my-files-v<version>-win-x64.msix` beside the
//! portable zip — the two ship side by side, the zip is NOT replaced. Signing the
//! `.msix` wrapper + attaching it happen in CI (release.yml); this verb only packs.
//!
//! We deliberately do NOT flip the csproj's `WindowsPackageType=None`
//! (CLAUDE.md / ADR-0016 forbid it): the package is produced out-of-band with the
//! standard Windows SDK tools (`MakePri` + `MakeAppx`), not by mutating the portable
//! build. Those tools ship inside the `Microsoft.Windows.SDK.BuildTools` `NuGet`
//! package the app already pins, so no ad-hoc toolchain install is needed.
//!
//! Hybrid, per ADR-0028: the package holds the apphost (`FindMyFiles.exe`, the one
//! registered executable), `fmf_engine.dll`, the `WinAppSDK` self-contained runtime,
//! AND `fmf-service.exe` as a plain CONTENT payload (NOT a `desktop6:Service` — that
//! extension cannot express our service-object DACL / privilege stripping / SID
//! capture, so the service stays a normal SCM service installed by our own elevated
//! helper, which runs without package identity). Bundling the service exe as content
//! is what lets the read-only-`WindowsApps` UI find an `fmf-service.exe` to copy into
//! `%ProgramData%` (ADR-0028 R3).

use crate::{cmd, fsx, paths, semver, version};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// The SSL.com Individual-Validation certificate Subject DN (ADR-0020). The
/// manifest's `Publisher` must equal this VERBATIM — App Installer derives install
/// trust from the signing chain only when the manifest publisher matches the cert
/// subject exactly (RESEARCH.md § MSIX). Kept in lock-step with the CI signer
/// assertion `SIGNER_SUBJECT_CONTAINS` (release.yml) so the manifest can never
/// drift from the certificate we actually sign with.
const PUBLISHER: &str = "CN=Yasunobu Sakashita";

/// Top-level payload files copied from the published `app/` that we intentionally
/// leave OUT of the package. `fmf.exe` (the CLI) ships out-of-band in the portable
/// zip (ADR-0028) — the installed app never invokes it. The native launcher
/// (`fmf-launcher.exe`) is a zip-only affordance (the "one obvious root exe") and
/// isn't in `app/` at all, so it needs no exclusion.
const PAYLOAD_EXCLUDE: &[&str] = &["fmf.exe"];

/// Files that MUST be in the staged payload for the package to launch + provision
/// the service. Mirrors `publish::REQUIRED` minus the CLI/launcher.
const REQUIRED: &[&str] = &["FindMyFiles.exe", "fmf_engine.dll", "fmf-service.exe"];

pub fn run(tag: Option<&str>) -> Result<()> {
    // MSIX ships stable only (numeric 4-part version has no pre-release form).
    let tag = tag.context(
        "package-msix requires a vX.Y.Z tag — MSIX packages carry a numeric \
         X.Y.Z.0 version and ship the stable channel only",
    )?;
    let base = semver::strip_tag_v(tag);
    semver::validate(base)?;
    let ver = version::msix_version(base)?; // X.Y.Z.0

    let app = paths::app_dir();
    if !app.exists() {
        bail!(
            "{} does not exist — run `just publish` first",
            app.display()
        );
    }

    let stage = paths::msix_stage_dir();
    fsx::force_remove_dir_all(&stage).with_context(|| format!("clean {}", stage.display()))?;
    fs::create_dir_all(&stage).with_context(|| format!("create {}", stage.display()))?;

    stage_payload(&app, &stage)?;
    write_manifest(&stage, &ver)?;
    copy_assets(&stage)?;
    generate_pri(&stage)?;

    let pkg = paths::package_dir();
    fs::create_dir_all(&pkg).with_context(|| format!("create {}", pkg.display()))?;
    let msix = pkg.join(format!("find-my-files-v{base}-win-x64.msix"));
    pack(&stage, &msix)?;

    println!(
        "package-msix: {} built (version {ver}, Publisher {PUBLISHER}). \
         Sign + attach in CI.",
        msix.display()
    );
    Ok(())
}

/// Copy the published `app/` payload into the package stage, minus the CLI. The
/// apphost stays the registered executable; `fmf_engine.dll` + the `WinAppSDK`
/// self-contained runtime (~100 files) + `FindMyFiles.pri` + `*.xbf` come along as
/// content, and `fmf-service.exe` rides as content payload (ADR-0028 R3).
fn stage_payload(app: &Path, stage: &Path) -> Result<()> {
    for (abs, rel) in fsx::collect_files(app).with_context(|| format!("walk {}", app.display()))? {
        // `rel` is forward-slash relative (fsx::collect_files); its first segment
        // is the top-level name. Windows `Path::join` accepts forward slashes, so
        // the relative form maps straight onto the stage tree.
        if PAYLOAD_EXCLUDE.contains(&rel.as_str()) {
            continue;
        }
        // Never bake runtime state into a read-only WindowsApps package: a
        // portable local run creates `<apphost>\data` (the AppPaths `<exe>\data`
        // root); a clean CI publish has none, but exclude it defensively so the
        // artifact is deterministic.
        if rel == "data" || rel.starts_with("data/") {
            continue;
        }
        let dest = stage.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::copy(&abs, &dest)
            .with_context(|| format!("copy {} -> {}", abs.display(), dest.display()))?;
    }

    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|f| !stage.join(f).exists())
        .collect();
    if !missing.is_empty() {
        bail!(
            "staged MSIX payload at {} is missing {missing:?} — the package would \
             not launch / could not provision the service",
            stage.display()
        );
    }
    Ok(())
}

/// Instantiate `Package.appxmanifest` from the checked-in template, substituting
/// the version and asserting the pinned Publisher (the install-trust prerequisite).
fn write_manifest(stage: &Path, version: &str) -> Result<()> {
    let tpl_path = manifest_template_path();
    let tpl = fs::read_to_string(&tpl_path)
        .with_context(|| format!("read manifest template {}", tpl_path.display()))?;

    // Guard the invariant App Installer trust depends on: a verbatim publisher.
    let expected = format!("Publisher=\"{PUBLISHER}\"");
    if !tpl.contains(&expected) {
        bail!(
            "{} must pin {expected} (the SSL.com IV cert Subject DN) — App Installer \
             derives install trust from a verbatim publisher/cert-subject match",
            tpl_path.display()
        );
    }

    let out = tpl.replace("{VERSION}", version);
    if out.contains("{VERSION}") {
        bail!("manifest template still has an unsubstituted {{VERSION}} placeholder");
    }
    // In a packed layout the manifest MUST be the footprint file `AppxManifest.xml`
    // (`Package.appxmanifest` is only the VS *source* name); MakeAppx rejects the
    // package without it.
    let dest = stage.join("AppxManifest.xml");
    fs::write(&dest, out).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

/// Copy the checked-in visual assets (`packaging/msix/Assets/`) into the stage.
fn copy_assets(stage: &Path) -> Result<()> {
    let src = packaging_dir().join("Assets");
    if !src.exists() {
        bail!(
            "{} not found — the MSIX visual assets (logos) must be generated first",
            src.display()
        );
    }
    let dst = stage.join("Assets");
    fsx::copy_dir_all(&src, &dst)
        .with_context(|| format!("copy assets {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

/// Generate the package's `resources.pri` with `MakePri`.
///
/// The published payload is a trap for a naive whole-folder index: the app's
/// compiled XAML (`*.xbf`) and localized strings are ALREADY inside
/// `FindMyFiles.pri`, yet the loose `*.xbf` files also sit in the tree — so the
/// folder-index pass and the PRI-merge pass both claim e.g. `Files/App.xbf` and
/// collide (PRI277). `foldernameAsQualifier` would additionally misread stray
/// dirs (`golden/`, the `WinAppSDK` `en-us/` MUI folder) as resource qualifiers.
///
/// So we index from a MINIMAL root holding only the manifest, the tile assets,
/// and the app's `FindMyFiles.pri`: the folder pass owns just the assets, the PRI
/// pass owns the app's compiled resources — no overlap. The framework component
/// PRIs (`Microsoft.UI*.pri`) stay loose in the real stage and load as components
/// exactly as they do for the unpackaged zip build. The resulting `resources.pri`
/// is written straight into the real payload stage.
fn generate_pri(stage: &Path) -> Result<()> {
    let makepri = sdk_tool("makepri")?;
    let makepri = path_arg(&makepri)?;

    // Minimal indexing root, a sibling of the stage so its own files never land
    // in the package.
    let priroot = stage.with_file_name("msix-priroot");
    fsx::force_remove_dir_all(&priroot).with_context(|| format!("clean {}", priroot.display()))?;
    fs::create_dir_all(&priroot).with_context(|| format!("create {}", priroot.display()))?;
    fs::copy(
        stage.join("AppxManifest.xml"),
        priroot.join("AppxManifest.xml"),
    )
    .context("stage manifest into pri root")?;
    fsx::copy_dir_all(&stage.join("Assets"), &priroot.join("Assets"))
        .context("stage assets into pri root")?;
    let app_pri = stage.join("FindMyFiles.pri");
    if !app_pri.exists() {
        bail!(
            "{} is missing — the published app PRI is required to merge the app's \
             strings + compiled XAML into the package resources",
            app_pri.display()
        );
    }
    fs::copy(&app_pri, priroot.join("FindMyFiles.pri")).context("stage app PRI into pri root")?;

    let config = priroot.join("priconfig.xml");
    let manifest = priroot.join("AppxManifest.xml");
    let pri = stage.join("resources.pri");
    let priroot_s = path_arg(&priroot)?;
    let config_s = path_arg(&config)?;
    let manifest_s = path_arg(&manifest)?;
    let pri_s = path_arg(&pri)?;

    cmd::run(
        &priroot,
        makepri,
        &["createconfig", "/cf", config_s, "/dq", "en-US", "/o"],
    )?;
    // Drop the <packaging> auto-resource-package block: it would split resources
    // into per-language / per-scale resource-pack PRIs (the `.msixbundle` model).
    // We ship ONE main `.msix`, so every qualifier must stay in the single
    // resources.pri — otherwise a split-out language would be absent from the
    // package entirely.
    strip_packaging_block(&config)?;
    cmd::run(
        &priroot,
        makepri,
        &[
            "new", "/pr", priroot_s, "/cf", config_s, "/mn", manifest_s, "/of", pri_s, "/o",
        ],
    )?;

    // The pri root was scratch — its manifest/assets/PRI copies must not end up
    // as package payload, so remove it wholesale (resources.pri already lives in
    // the real stage).
    fsx::force_remove_dir_all(&priroot).with_context(|| format!("clean {}", priroot.display()))?;
    Ok(())
}

/// Pack the staged layout into the `.msix` with `MakeAppx`.
fn pack(stage: &Path, msix: &Path) -> Result<()> {
    let makeappx = sdk_tool("makeappx")?;
    let makeappx = path_arg(&makeappx)?;
    let stage_s = path_arg(stage)?;
    let msix_s = path_arg(msix)?;
    cmd::run(
        stage,
        makeappx,
        &["pack", "/d", stage_s, "/p", msix_s, "/o"],
    )?;
    Ok(())
}

/// Remove the `<packaging>…</packaging>` auto-resource-package block `MakePri`'s
/// `createconfig` emits, so `new` produces one monolithic `resources.pri` (single
/// `.msix`) rather than per-language/scale resource-pack splits (`.msixbundle`).
fn strip_packaging_block(config: &Path) -> Result<()> {
    const OPEN: &str = "<packaging>";
    const CLOSE: &str = "</packaging>";
    let xml = fs::read_to_string(config).with_context(|| format!("read {}", config.display()))?;
    if let (Some(start), Some(end)) = (xml.find(OPEN), xml.find(CLOSE)) {
        let mut out = String::with_capacity(xml.len());
        out.push_str(xml[..start].trim_end_matches([' ', '\t']));
        out.push_str(&xml[end + CLOSE.len()..]);
        fs::write(config, out).with_context(|| format!("write {}", config.display()))?;
    }
    Ok(())
}

fn packaging_dir() -> PathBuf {
    paths::repo_root().join("packaging").join("msix")
}

fn manifest_template_path() -> PathBuf {
    packaging_dir().join("Package.appxmanifest.in")
}

/// Borrow a path as a UTF-8 `&str` for the argv, failing loudly on the rare
/// non-UTF-8 path rather than lossily mangling a tool argument.
fn path_arg(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", p.display()))
}

/// Locate a Windows SDK tool (`makeappx` / `makepri` / `signtool`), preferring the
/// pinned `Microsoft.Windows.SDK.BuildTools` `NuGet` package (deterministic, no
/// ad-hoc install — CLAUDE.md compliant) and falling back to the newest installed
/// Windows 10 SDK under `Windows Kits\10\bin` (present on GitHub `windows-latest`).
fn sdk_tool(name: &str) -> Result<PathBuf> {
    let exe = format!("{name}.exe");
    if let Some(p) = newest_tool_in_nuget(&exe) {
        return Ok(p);
    }
    if let Some(p) = newest_tool_in_windows_kits(&exe) {
        return Ok(p);
    }
    bail!(
        "could not locate {exe} — expected it in the Microsoft.Windows.SDK.BuildTools \
         NuGet package (pinned in app/FindMyFiles/FindMyFiles.csproj; run `dotnet \
         restore` once) or in an installed Windows 10 SDK"
    );
}

/// `<nuget>/microsoft.windows.sdk.buildtools/<pkgver>/bin/<sdkver>/x64/<exe>`,
/// newest package + SDK version wins.
fn newest_tool_in_nuget(exe: &str) -> Option<PathBuf> {
    let base = nuget_packages_root()?.join("microsoft.windows.sdk.buildtools");
    for pkgver in sorted_subdirs_desc(&base) {
        let bin = pkgver.join("bin");
        for sdkver in sorted_subdirs_desc(&bin) {
            let candidate = sdkver.join("x64").join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `<ProgramFiles(x86)>\Windows Kits\10\bin\<sdkver>\x64\<exe>` (newest), with a
/// last-ditch `…\bin\x64\<exe>` for older SDK layouts.
fn newest_tool_in_windows_kits(exe: &str) -> Option<PathBuf> {
    let pf = std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles"))?;
    let bin = PathBuf::from(pf)
        .join("Windows Kits")
        .join("10")
        .join("bin");
    for sdkver in sorted_subdirs_desc(&bin) {
        let candidate = sdkver.join("x64").join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let direct = bin.join("x64").join(exe);
    direct.is_file().then_some(direct)
}

fn nuget_packages_root() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("NUGET_PACKAGES") {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    let home = std::env::var_os("USERPROFILE")?;
    Some(PathBuf::from(home).join(".nuget").join("packages"))
}

/// Immediate subdirectories of `dir`, sorted descending by name so the newest
/// version dir (e.g. `10.0.26100.0`) comes first. Missing dir → empty.
fn sorted_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    dirs.reverse();
    dirs
}
