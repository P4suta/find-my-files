//! `xtask publish [--skip-rust]` — assemble the distributable bundle in
//! build/dist/FindMyFiles.
//!
//! Publishes the app (not a bare `dotnet build` — only the publish output wires
//! WinRT.Runtime.dll, the `WinAppSDK` native helpers and the compiled XAML into a
//! runnable bundle), prunes the locale dirs the app doesn't ship and the dead-
//! weight artifacts it never loads (PDB / XML doc / design-time + `WebView2` DLLs,
//! see the `prune` module), copies the engine binaries, then SELF-VERIFIES the
//! result. The self-check is what lets
//! us drop ci.yml's separate "verify bundle is runnable" step: the producer of
//! the bundle guarantees its own output instead of a downstream guard.
//!
//! `--skip-rust true` skips the in-build cargo step (CI prebuilds + downloads
//! the engine binaries into build/engine/release/ before this runs).

use crate::{
    cmd, fsx, locale, notices, paths, pe_digest, pe_load, prune, semver, version, win_version,
};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Engine binaries copied in alongside the published app.
const ENGINE_BINS: &[&str] = &["fmf-service.exe"];

/// Files whose presence (inside `app/`) means the bundle can actually launch.
/// `FindMyFiles.exe` (the apphost), its managed entry assembly, dependency/runtime
/// manifests, compiled resource index and `WinRT.Runtime.dll` come from `dotnet
/// publish`; `fmf_engine.dll` comes via the csproj `<None Include>`, and the
/// service executable is copied below. The root-level launcher is verified separately
/// (it is what the user double-clicks; the apphost is its target). Compiled XAML
/// is verified separately against every source `.xaml` file, so adding a view
/// automatically extends the structural gate.
///
/// `coreclr.dll` / `hostfxr.dll` are the proof the .NET runtime is actually
/// bundled (self-contained). `WinRT.Runtime.dll` alone is NOT enough — it also
/// ships in a framework-dependent build — so without these the bundle would
/// launch only where a matching .NET is already installed and demand a runtime
/// download everywhere else. Guarding here keeps the `SelfContained` regression
/// (see `FindMyFiles.csproj`) from ever shipping green again.
const REQUIRED: &[&str] = &[
    "FindMyFiles.exe",
    "FindMyFiles.dll",
    "FindMyFiles.deps.json",
    "FindMyFiles.runtimeconfig.json",
    // The Windows App SDK names the application resource PRI after TargetName;
    // for this project that is FindMyFiles.pri (not a generic resources.pri).
    "FindMyFiles.pri",
    "WinRT.Runtime.dll",
    "coreclr.dll",
    "hostfxr.dll",
    "fmf_engine.dll",
    "fmf-service.exe",
];

/// First-party PEs we Authenticode-sign, as `(path relative to the bundle root,
/// unique name in the flat signing dir)`. This is the single source of truth the
/// release workflow's `sign-stage` / `sign-collect` steps drive — the map used
/// to live duplicated in two inline-PowerShell blocks.
///
/// NOT the same set as [`REQUIRED`]: this signs the root launcher (what the user
/// double-clicks) and excludes Microsoft-signed `WinRT.Runtime.dll` (re-signing
/// it would waste eSigner quota and claim authorship we don't have). The root
/// launcher and the `app\` apphost share the basename `FindMyFiles.exe`, so a
/// flat copy-by-basename would collide — each gets a unique stage name.
/// Authenticode lives inside the PE, so staging under a different filename and
/// mapping back afterwards is safe.
pub const FIRST_PARTY_PES: &[(&str, &str)] = &[
    ("FindMyFiles.exe", "FindMyFiles.exe"),
    ("app/FindMyFiles.exe", "app-FindMyFiles.exe"),
    ("app/FindMyFiles.dll", "app-FindMyFiles.dll"),
    ("app/fmf-service.exe", "fmf-service.exe"),
    ("app/fmf_engine.dll", "fmf_engine.dll"),
];

/// The native launcher built in the engine workspace, copied to the bundle root
/// as [`ENTRY_EXE`] — the single file a user is meant to run. It spawns
/// `app/FindMyFiles.exe`, forwarding arguments (see the `fmf-launcher` crate).
const LAUNCHER_BIN: &str = "fmf-launcher.exe";
/// Shipped name of the launcher at the bundle root (intentionally the same as
/// the apphost inside `app/` — the user sees one obvious `FindMyFiles.exe`).
const ENTRY_EXE: &str = "FindMyFiles.exe";
const TEST_SEAM_MARKER: &str = "FMF_TEST_SEAMS";
const TEST_SEAM_FIXTURE: &str = "golden/invalid_queries.json";
const FORBIDDEN_TEST_DIR_NAMES: &[&str] = &["golden", "fixture", "fixtures", "test", "tests"];

pub fn run(skip_rust: bool) -> Result<()> {
    run_bundle(skip_rust, BundleKind::Shipping)
}

pub fn run_ui_test(skip_rust: bool) -> Result<()> {
    run_bundle(skip_rust, BundleKind::UiTest)
}

#[derive(Clone, Copy)]
enum BundleKind {
    Shipping,
    UiTest,
}

impl BundleKind {
    fn paths(self) -> (PathBuf, PathBuf) {
        match self {
            Self::Shipping => (paths::dist_dir(), paths::app_dir()),
            Self::UiTest => (paths::ui_test_dist_dir(), paths::ui_test_app_dir()),
        }
    }

    const fn test_seams(self) -> bool {
        matches!(self, Self::UiTest)
    }

    const fn artifact_kind(self) -> &'static str {
        match self {
            Self::Shipping => "shipping",
            Self::UiTest => "ui-test",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Shipping => "publish",
            Self::UiTest => "publish-ui-test",
        }
    }
}

fn run_bundle(skip_rust: bool, kind: BundleKind) -> Result<()> {
    let root = paths::repo_root();
    let (dist, app) = kind.paths();
    // Resolve and validate the immutable source identity before removing or
    // rebuilding anything. CI may pin the exact commit; otherwise local builds
    // use full HEAD while their separate version display remains short/dirty.
    let source_commit = version::resolve_source_commit()?;

    // Clean the whole stale bundle (launcher + README + app/ payload). Under CI
    // a fresh runner always cleans cleanly, so a failure there signals a real
    // problem (a stale lock / leftover we must not silently publish over) — fail
    // closed. Locally it stays best-effort: a running app can legitimately lock
    // the old bundle and `dotnet publish` overwrites anyway (the self-verify at
    // the end is the real gate), so we warn rather than fail as the old recipe did.
    fsx::force_remove_dir_all(&dist).with_context(|| {
        format!(
            "clean {} — refusing to publish over leftovers",
            dist.display()
        )
    })?;

    // Build/downloaded engine binaries must exist before compiling the app:
    // the Authenticode-stable digest of the exact service image is embedded in
    // the managed assembly and enforced immediately before every elevation.
    build_engine_bins_if_needed(skip_rust)?;
    let service_source = paths::engine_release_dir().join("fmf-service.exe");
    pe_load::require_system32_only(&service_source).with_context(|| {
        format!(
            "verify source service's System32-only dependent-load policy at {}",
            service_source.display()
        )
    })?;
    let service_digest = pe_digest::sha256_file(&service_source).with_context(|| {
        format!(
            "derive service image identity from {}",
            service_source.display()
        )
    })?;

    // Publish the self-contained app into the `app/` subfolder — the bundle root
    // is reserved for the launcher + README so "which exe do I run" is obvious.
    // Pass the absolute path so the output location is the single source in
    // paths::app_dir(), independent of `cmd::run`'s working directory.
    let app_arg = app.to_str().context("app path is not valid UTF-8")?;
    let skip_arg = format!("-p:SkipRustBuild={skip_rust}");
    let seam_arg = format!("-p:FmfTestSeams={}", kind.test_seams());
    let artifact_arg = format!("-p:FmfArtifactKind={}", kind.artifact_kind());
    let service_digest_arg = format!("-p:FmfServiceImageSha256={service_digest}");
    let mut args = vec![
        "publish",
        "app/FindMyFiles",
        "-c",
        "Release",
        "-r",
        "win-x64",
        "-o",
        app_arg,
        &skip_arg,
        &seam_arg,
        &artifact_arg,
        &service_digest_arg,
    ];
    // Every distributable is built from the pinned dependency graph. Publishing
    // over a stale lock file is a supply-chain error locally as well as in CI;
    // dependency edits must update and review packages.lock.json first. The
    // MSBuild property reaches the implicit restore performed by `dotnet
    // publish` (more robust than the CLI flag across SDK versions).
    args.push("-p:RestoreLockedMode=true");
    cmd::run(&root, "dotnet", &args)?;
    verify_test_artifacts(&app, kind.test_seams())?;

    prune_locales(&app)?;
    prune_publish_artifacts(&app)?;
    copy_engine_bins(&app)?;
    let copied_service_digest = pe_digest::sha256_file(&app.join("fmf-service.exe"))?;
    if copied_service_digest != service_digest {
        bail!(
            "copied fmf-service.exe image identity drifted: expected {service_digest}, \
             got {copied_service_digest}"
        );
    }
    pe_load::require_system32_only(&app.join("fmf-service.exe")).with_context(|| {
        format!(
            "verify copied service's System32-only dependent-load policy at {}",
            app.join("fmf-service.exe").display()
        )
    })?;
    verify_bundle(&app, &root.join("app/FindMyFiles"))?;
    let build_version = version::resolve_bundle_version()?;
    place_launcher_and_readme(&dist)?;
    place_buildinfo(&dist, &build_version, source_commit.as_deref())?;
    verify_bundle_identity(&dist, &build_version, source_commit.as_deref())?;
    place_legal_notices(&root, &dist)?;

    println!(
        "{}: {} assembled and verified \
         (root launcher + LICENSE.txt + THIRD-PARTY-NOTICES.txt + app/ with {} \
         required files).",
        kind.label(),
        dist.display(),
        REQUIRED.len()
    );
    Ok(())
}

pub fn verify_test_artifacts(app: &Path, expected: bool) -> Result<()> {
    let marker = app.join(TEST_SEAM_MARKER);
    if expected {
        if !is_nonempty_file(&marker) {
            bail!(
                "UI-test publish did not emit required seam marker {}",
                marker.display()
            );
        }
        let fixture = app.join(TEST_SEAM_FIXTURE);
        if !is_nonempty_file(&fixture) {
            bail!(
                "UI-test publish did not emit required non-empty fixture {}",
                fixture.display()
            );
        }
        return Ok(());
    }

    if marker.exists() {
        bail!(
            "shipping bundle contains forbidden test-seam marker {}",
            marker.display()
        );
    }
    let mut forbidden_dirs = Vec::new();
    collect_forbidden_test_dirs(app, app, &mut forbidden_dirs)?;
    if !forbidden_dirs.is_empty() {
        forbidden_dirs.sort();
        bail!("shipping bundle contains forbidden test/fixture directories {forbidden_dirs:?}");
    }
    Ok(())
}

fn collect_forbidden_test_dirs(
    root: &Path,
    current: &Path,
    forbidden: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(current).with_context(|| format!("inspect {}", current.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if FORBIDDEN_TEST_DIR_NAMES
            .iter()
            .any(|candidate| name.to_string_lossy().eq_ignore_ascii_case(candidate))
        {
            forbidden.push(
                path.strip_prefix(root)
                    .context("test directory escaped publish root")?
                    .to_path_buf(),
            );
            continue;
        }
        collect_forbidden_test_dirs(root, &path, forbidden)?;
    }
    Ok(())
}

/// Remove `WinAppSDK` locale dirs the app doesn't ship (lookups fall back to the
/// neutral resources). Collect first, then delete — don't mutate the directory
/// mid-enumeration.
fn prune_locales(app: &Path) -> Result<()> {
    let mut to_prune = Vec::new();
    for entry in fs::read_dir(app).with_context(|| format!("read {}", app.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if locale::should_prune_locale_dir(&entry.file_name().to_string_lossy()) {
            to_prune.push(entry.path());
        }
    }
    for dir in to_prune {
        fsx::force_remove_dir_all(&dir)
            .with_context(|| format!("prune locale {}", dir.display()))?;
    }
    Ok(())
}

/// Strip the dead-weight publish artifacts (see [`prune`]) — files `dotnet
/// publish` copies in that the running app never loads. Tolerant of absence: a
/// file we mean to drop simply not being there (an SDK that stops emitting it)
/// is success, not an error — `verify_bundle` is the gate that a listed file is
/// actually gone. Only files are removed; the set is all `app/`-root basenames.
fn prune_publish_artifacts(app: &Path) -> Result<()> {
    let mut removed = 0u32;
    for rel in prune::shipped_prune_set() {
        let path = app.join(rel);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("prune {}", path.display()))?;
            removed += 1;
        }
    }
    println!("publish: pruned {removed} unused artifact(s) from app/");
    Ok(())
}

/// Build the standalone engine binaries when the caller did NOT prebuild them.
///
/// With `skip_rust=false` the only Rust that ran is the csproj's `BuildRustEngine`
/// target, which builds `-p fmf-ffi` for `fmf_engine.dll` alone — NOT the service
/// executable or the launcher. Without this, running
/// `just publish-app` on its own (its default is `skip_rust=false`) would sail
/// through `dotnet publish` and then bail in `copy_engine_bins` on the absent
/// service. Build the whole engine workspace (same as `just build`) so it, and
/// the launcher `place_launcher_and_readme` copies, all exist.
///
/// CI passes `skip_rust=true` (it prebuilds + downloads the bins into
/// `build/engine/release/`), so this is inert on the shipping path — it only
/// smooths the local single-recipe invocation.
fn build_engine_bins_if_needed(skip_rust: bool) -> Result<()> {
    if skip_rust {
        return Ok(());
    }
    // Run from engine/ (not `--manifest-path`) so its `.cargo/config.toml`
    // redirects the target dir under build/engine (ADR-0021), matching
    // `paths::engine_release_dir()` that the copy/launcher steps read from.
    cmd::run(&paths::engine_dir(), "cargo", &["build", "--release"])
}

fn copy_engine_bins(app: &Path) -> Result<()> {
    let release = paths::engine_release_dir();
    for bin in ENGINE_BINS {
        let src = release.join(bin);
        let target = app.join(bin);
        fs::copy(&src, &target)
            .with_context(|| format!("copy {} -> {}", src.display(), target.display()))?;
    }
    Ok(())
}

fn verify_bundle(app: &Path, xaml_source: &Path) -> Result<()> {
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|f| !is_nonempty_file(&app.join(f)))
        .collect();
    if !missing.is_empty() {
        bail!(
            "bundle at {} is missing or has empty startup-critical files {missing:?} — \
             it would not launch",
            app.display()
        );
    }

    let expected_xbf = expected_xbf_paths(xaml_source)?;
    let missing_xbf: Vec<PathBuf> = expected_xbf
        .into_iter()
        .filter(|rel| !is_nonempty_file(&app.join(rel)))
        .collect();
    if !missing_xbf.is_empty() {
        bail!(
            "bundle at {} is missing or has empty compiled XAML {missing_xbf:?} — \
             the published WinUI app would fail while loading a page/resource",
            app.display()
        );
    }

    // Negative allowlist: the dead-weight artifacts we prune must be gone. Same
    // philosophy as the missing-file check — the producer guarantees its output.
    // Catches an SDK update reintroducing a stripped file (e.g. a renamed
    // WebView2 assembly) that the tolerant pruner would silently miss.
    let leftover: Vec<&str> = prune::shipped_prune_set()
        .filter(|f| app.join(f).exists())
        .collect();
    if !leftover.is_empty() {
        bail!(
            "bundle at {} still ships pruned dead weight {leftover:?} — the prune \
             list drifted from what publish emits",
            app.display()
        );
    }
    Ok(())
}

/// A startup artifact must be a real, non-empty file. `Path::exists` alone would
/// accept an accidentally-created directory or zero-byte placeholder and let a
/// structurally broken bundle through the producer-side release gate.
fn is_nonempty_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// Derive the compiled-XAML contract from the application source tree instead of
/// keeping a second handwritten list. Every checked-in `.xaml` maps to a
/// bundle-relative `.xbf` at the same path. Generated `obj/` and `bin/` trees are
/// excluded so a previous build can never manufacture extra expectations.
///
/// An empty source set is an error: otherwise a bad source path would make the
/// check pass vacuously and silently stop verifying XBF altogether.
fn expected_xbf_paths(source_root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, dir: &Path, expected: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name != "obj" && name != "bin" {
                    visit(root, &path, expected)?;
                }
                continue;
            }
            if !file_type.is_file()
                || !path
                    .extension()
                    .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("xaml"))
            {
                continue;
            }
            let mut rel = path
                .strip_prefix(root)
                .with_context(|| {
                    format!(
                        "derive XBF path: {} is outside {}",
                        path.display(),
                        root.display()
                    )
                })?
                .to_path_buf();
            rel.set_extension("xbf");
            expected.push(rel);
        }
        Ok(())
    }

    let mut expected = Vec::new();
    visit(source_root, source_root, &mut expected)
        .with_context(|| format!("derive compiled XAML set from {}", source_root.display()))?;
    expected.sort();
    if expected.is_empty() {
        bail!(
            "{} contains no source XAML — refusing to verify zero compiled XBF files",
            source_root.display()
        );
    }
    Ok(expected)
}

/// Put the user-facing entry point at the bundle root: the native launcher
/// (renamed to `FindMyFiles.exe`) plus a short `README.txt`. The launcher is the
/// only executable a user should need to find; everything else lives in `app/`.
fn place_launcher_and_readme(dist: &Path) -> Result<()> {
    // Copy the native launcher to the root as FindMyFiles.exe — the one file a
    // user double-clicks. It spawns app/FindMyFiles.exe (verified above).
    let src = paths::engine_release_dir().join(LAUNCHER_BIN);
    let entry = dist.join(ENTRY_EXE);
    fs::copy(&src, &entry)
        .with_context(|| format!("copy {} -> {}", src.display(), entry.display()))?;

    // CRLF + a UTF-8 BOM so Notepad renders it correctly, the Japanese half too.
    let readme = format!("\u{feff}{}", README.replace('\n', "\r\n"));
    fs::write(dist.join("README.txt"), readme).context("write README.txt")?;

    // Self-verify the user-facing entry point — the producer guarantees a bundle
    // with an obvious thing to run, not a downstream guard.
    if !entry.exists() {
        bail!(
            "launcher {} is missing — the bundle has no obvious entry point",
            entry.display()
        );
    }
    Ok(())
}

/// Drop `BUILDINFO.txt` at the bundle root so a downloaded copy stays
/// identifiable after the zip name is lost on extraction: which channel, which
/// version, which commit — readable in Notepad and grep-able by tooling. The
/// version label uses the SAME precedence as the shipped binaries
/// (`FMF_BUILD_VERSION`, else the local `-dev+g<short-sha>` default). Its
/// independent `commit:` field carries the exact full source identity from
/// `FMF_SOURCE_COMMIT` or local HEAD.
fn place_buildinfo(dist: &Path, full: &str, source_commit: Option<&str>) -> Result<()> {
    let commit_date = version::git_commit_date();
    let body = version::render_buildinfo(full, commit_date.as_deref(), source_commit)?;
    // Same Notepad-friendly encoding as README.txt: UTF-8 BOM + CRLF.
    let text = format!("\u{feff}{}", body.replace('\n', "\r\n"));
    fs::write(dist.join("BUILDINFO.txt"), text).context("write BUILDINFO.txt")?;
    Ok(())
}

/// Prove the user-visible launcher metadata and the adjacent BUILDINFO describe
/// the exact same build identity. `ProductVersion` carries the full
/// channel/sha identity, while Win32 `FileVersion` carries its canonical clean
/// `X.Y.Z` base. The build script already fails closed if the resource could not
/// be embedded; this post-build readback catches a stale/wrong stamped launcher.
fn verify_bundle_identity(
    dist: &Path,
    expected_full: &str,
    expected_source_commit: Option<&str>,
) -> Result<()> {
    let buildinfo_path = dist.join("BUILDINFO.txt");
    let buildinfo = fs::read_to_string(&buildinfo_path)
        .with_context(|| format!("read {}", buildinfo_path.display()))?;
    let buildinfo_version = parse_buildinfo_version(&buildinfo)?;
    if let Some(expected_commit) = expected_source_commit {
        let actual_commit = version::parse_buildinfo_source_commit(&buildinfo)?;
        if actual_commit != expected_commit {
            bail!(
                "bundle source identity drift: BUILDINFO commit is '{actual_commit}', \
                 expected '{expected_commit}'"
            );
        }
    }
    let launcher = win_version::read(&dist.join(ENTRY_EXE))?;
    validate_bundle_identity(
        expected_full,
        &launcher.product_version,
        &launcher.file_version,
        buildinfo_version,
    )
}

fn parse_buildinfo_version(source: &str) -> Result<&str> {
    let versions: Vec<&str> = source
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .filter_map(|line| line.strip_prefix("version:").map(str::trim))
        .collect();
    match versions.as_slice() {
        [version] if !version.is_empty() => Ok(version),
        [] => bail!("BUILDINFO.txt has no version field"),
        [_] => bail!("BUILDINFO.txt has an empty version field"),
        _ => bail!(
            "BUILDINFO.txt has {} version fields; exactly one is required",
            versions.len()
        ),
    }
}

fn validate_bundle_identity(
    expected_full: &str,
    launcher_product: &str,
    launcher_file: &str,
    buildinfo: &str,
) -> Result<()> {
    let base = expected_full
        .split(['-', '+'])
        .next()
        .context("build version has no base version")?;
    semver::validate(base).context("build version has a non-canonical base")?;
    for (source, actual, expected) in [
        ("launcher ProductVersion", launcher_product, expected_full),
        ("launcher FileVersion", launcher_file, base),
        ("BUILDINFO version", buildinfo, expected_full),
    ] {
        if actual != expected {
            bail!("bundle identity drift: {source} is '{actual}', expected '{expected}'");
        }
    }
    Ok(())
}

/// Shipped name of the project license at the bundle root — copied verbatim from
/// the repo `LICENSE` so it is byte-identical to the governing text.
const LICENSE_FILE: &str = "LICENSE.txt";
/// Shipped name of the generated third-party attribution file at the bundle root.
const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES.txt";

/// Ship the legal texts the bundle must carry to redistribute its dependencies
/// (a release gate): the project's own Apache-2.0 `LICENSE.txt`, and a generated
/// `THIRD-PARTY-NOTICES.txt` attributing every redistributed third party.
///
/// - `LICENSE.txt` is a verbatim copy of the repo `LICENSE` (Apache-2.0 §4 wants
///   the license text to travel with the binaries; a byte copy is the most
///   defensible form for a legal file — no BOM/CRLF rewriting).
/// - `THIRD-PARTY-NOTICES.txt` = the curated .NET/NuGet section (see `notices`)
///   plus the `cargo-about` render of the Rust crate graph. Encoded BOM + CRLF
///   like README/BUILDINFO so Notepad renders it.
///
/// Self-verifies both landed — the producer guarantees a legally shippable
/// bundle rather than leaning on a downstream guard.
fn place_legal_notices(root: &Path, dist: &Path) -> Result<()> {
    // Verbatim copy of the governing license — no re-encoding.
    let src = root.join("LICENSE");
    let license = dist.join(LICENSE_FILE);
    fs::copy(&src, &license)
        .with_context(|| format!("copy {} -> {}", src.display(), license.display()))?;

    let rust = generate_rust_notices()?;
    let body = notices::assemble(&rust);
    // Same Notepad-friendly encoding as README/BUILDINFO: UTF-8 BOM + CRLF.
    let text = format!("\u{feff}{}", body.replace('\n', "\r\n"));
    let notices_path = dist.join(NOTICES_FILE);
    fs::write(&notices_path, text).with_context(|| format!("write {}", notices_path.display()))?;

    for f in [LICENSE_FILE, NOTICES_FILE] {
        if !dist.join(f).exists() {
            bail!(
                "bundle at {} is missing {f} — it may not be redistributed \
                 without its license/attribution texts",
                dist.display()
            );
        }
    }
    Ok(())
}

/// Render the Rust half of the third-party notices by driving `cargo-about` over
/// the engine workspace (config + template committed at `engine/about.toml` /
/// `engine/about.hbs`) and reading back its output for `notices::assemble`.
///
/// Fetch the complete locked graph first, then run `cargo-about` with
/// `--offline --locked`. A release build only fetches dependencies needed by its
/// compiled targets; `cargo-about` asks Cargo for workspace metadata, which also
/// needs sources such as dev/build dependencies. Making that precondition
/// explicit here keeps every caller (local, CI, nightly and stable) correct on a
/// cold runner. The render itself remains reproducible and independent of
/// clearlydefined.io or any other third-party service.
///
/// Local development provisions `cargo-about` from the `mise.toml` pin; CI jobs
/// install the same pinned release directly.
///
/// Output goes through `-o <file>`, not stdout: cargo-about refuses a redirected
/// stdout on Windows (PowerShell re-encodes piped bytes to UTF-16 and corrupts
/// the license texts), so it writes a UTF-8 file we read back.
fn generate_rust_notices() -> Result<String> {
    let engine = paths::engine_dir();
    cmd::run(&engine, "cargo", &["fetch", "--locked"]).context(
        "fetch the complete locked Cargo graph required for offline third-party notice generation",
    )?;
    let out_file =
        std::env::temp_dir().join(format!("fmf-third-party-rust-{}.txt", std::process::id()));
    let out_arg = out_file
        .to_str()
        .context("temp notices path is not valid UTF-8")?;
    let output = std::process::Command::new("cargo-about")
        .args([
            "generate",
            "--offline",
            "--locked",
            "--manifest-path",
            "crates/fmf-service/Cargo.toml",
            "-c",
            "about.toml",
            "about.hbs",
            "-o",
            out_arg,
        ])
        .current_dir(&engine)
        .output()
        .context(
            "failed to spawn `cargo-about` (is it on PATH? it is pinned in \
             mise.toml — run `mise install`)",
        )?;
    if !output.status.success() {
        bail!(
            "`cargo-about generate` exited with {} — stderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let body =
        fs::read_to_string(&out_file).with_context(|| format!("read {}", out_file.display()))?;
    // Best-effort cleanup — a stale temp file is harmless and the next run's
    // pid-tagged name won't collide.
    let _ = fs::remove_file(&out_file);
    // cargo-about's -o writes CRLF on Windows; normalize to LF so the single
    // CRLF re-encoding in `place_legal_notices` doesn't double the carriage
    // returns (which Notepad renders as stray blank lines).
    Ok(body.replace('\r', ""))
}

/// End-user README dropped at the bundle root, beside the launcher (English then
/// Japanese — the app ships both locales). Stored as LF; written as CRLF + BOM.
const README: &str = "\
FindMyFiles — fast filename search for Windows
==============================================

>> To start: double-click  FindMyFiles.exe  (here, in this folder).

The app files live in  app\\ . On first launch, choose Enable Search and accept the one Windows administrator prompt.
This registers the on-demand search service;
the app itself stays unprivileged and starts the service only when needed.

Data is stored outside this extracted folder:
  %ProgramData%\\find-my-files\\  service, index, service settings, engine logs
  %APPDATA%\\find-my-files\\      UI settings and app log

Full uninstall:
  1. Open the gear menu > Manage service.
  2. Choose \"Remove service and all data...\", confirm, and accept the UAC prompt.
     FindMyFiles removes the service and scheduled task, both data directories
     above, and then closes.
  3. Delete this extracted folder.

Licensing: FindMyFiles is Apache-2.0 — full text in  LICENSE.txt  (here). The
third-party code it bundles (.NET runtime, Windows App SDK, Rust crates, ...) is
attributed in  THIRD-PARTY-NOTICES.txt  (here).

Apache-2.0  -  https://github.com/P4suta/find-my-files

--------------------------------------------------------------------------

FindMyFiles — Windows 向け 高速ファイル名検索
==============================================

>> 起動: このフォルダーの  FindMyFiles.exe  をダブルクリック。

アプリ本体は  app\\  にあります。初回起動時に「検索を有効にする」を選び、
Windows の管理者許可を1回承認します。これでオンデマンド検索サービスが登録
されます。アプリ自体は非特権のままで、必要なときだけサービスを起動します。

データは展開先フォルダーの外に保存されます:
  %ProgramData%\\find-my-files\\  サービス、索引、サービス設定、エンジンログ
  %APPDATA%\\find-my-files\\      UI 設定、アプリログ

完全にアンインストールするには:
  1. 歯車メニュー >「サービスの管理」を開きます。
  2.「サービスとすべてのデータを削除…」を選んで確認し、UAC を承認します。
     サービスとスケジュールタスク、上記2つのデータフォルダーが削除され、
     FindMyFiles は終了します。
  3. この展開先フォルダーを削除します。

ライセンス: FindMyFiles は Apache-2.0 です(全文はこのフォルダーの  LICENSE.txt )。
同梱する第三者コード(.NET ランタイム/Windows App SDK/Rust クレート ほか)の
帰属表示はこのフォルダーの  THIRD-PARTY-NOTICES.txt  にあります。

Apache-2.0  -  https://github.com/P4suta/find-my-files
";

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("xtask-publish-{tag}-{}", std::process::id()))
    }

    fn write_nonempty(path: &Path) {
        fs::create_dir_all(path.parent().expect("fixture path has a parent")).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn complete_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = scratch(tag);
        let _ = fsx::force_remove_dir_all(&base);
        let app = base.join("app");
        let source = base.join("source");

        for required in REQUIRED {
            write_nonempty(&app.join(required));
        }
        for rel in ["App.xaml", "MainPage.xaml", "Views/SettingsDialog.xaml"] {
            write_nonempty(&source.join(rel));
            write_nonempty(&app.join(rel).with_extension("xbf"));
        }
        (base, app, source)
    }

    #[test]
    fn complete_startup_structure_is_accepted() {
        let (base, app, source) = complete_fixture("complete");
        verify_bundle(&app, &source).unwrap();
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn ui_test_publish_requires_marker_and_real_fixture() {
        let base = scratch("ui-test-artifacts");
        let _cleanup = fsx::force_remove_dir_all(&base);
        let app = base.join("app");
        fs::create_dir_all(&app).unwrap();

        assert!(verify_test_artifacts(&app, true).is_err());
        fs::write(app.join(TEST_SEAM_MARKER), b"enabled\n").unwrap();
        assert!(verify_test_artifacts(&app, true).is_err());
        write_nonempty(&app.join(TEST_SEAM_FIXTURE));
        assert!(verify_test_artifacts(&app, true).is_ok());
        fs::write(app.join(TEST_SEAM_MARKER), b"").unwrap();
        assert!(verify_test_artifacts(&app, true).is_err());

        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn shipping_publish_rejects_marker_and_any_fixture_directory() {
        let base = scratch("shipping-test-artifacts");
        let _cleanup = fsx::force_remove_dir_all(&base);
        let app = base.join("app");
        fs::create_dir_all(&app).unwrap();

        assert!(verify_test_artifacts(&app, false).is_ok());
        fs::write(app.join(TEST_SEAM_MARKER), b"enabled\n").unwrap();
        assert!(verify_test_artifacts(&app, false).is_err());
        fs::remove_file(app.join(TEST_SEAM_MARKER)).unwrap();

        fs::create_dir_all(app.join("golden")).unwrap();
        assert!(
            verify_test_artifacts(&app, false).is_err(),
            "even an empty golden directory is a shipping boundary violation"
        );
        fsx::force_remove_dir_all(&app.join("golden")).unwrap();

        fs::create_dir_all(app.join("payload/Tests")).unwrap();
        assert!(
            verify_test_artifacts(&app, false).is_err(),
            "test directories are forbidden at every depth and case"
        );

        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn every_fixed_startup_file_is_required_and_nonempty() {
        let (base, app, source) = complete_fixture("required");
        for required in REQUIRED {
            let path = app.join(required);
            fs::remove_file(&path).unwrap();
            assert!(
                verify_bundle(&app, &source).is_err(),
                "missing {required} must fail closed"
            );
            fs::write(&path, b"").unwrap();
            assert!(
                verify_bundle(&app, &source).is_err(),
                "empty {required} must fail closed"
            );
            fs::write(&path, b"x").unwrap();
        }
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn every_source_xaml_requires_its_matching_nonempty_xbf() {
        let (base, app, source) = complete_fixture("xbf");
        let nested = app.join("Views/SettingsDialog.xbf");
        fs::remove_file(&nested).unwrap();
        assert!(verify_bundle(&app, &source).is_err());
        fs::write(&nested, b"").unwrap();
        assert!(verify_bundle(&app, &source).is_err());
        fs::write(&nested, b"x").unwrap();
        verify_bundle(&app, &source).unwrap();
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn generated_xaml_trees_do_not_create_publish_requirements() {
        let (base, app, source) = complete_fixture("generated");
        write_nonempty(&source.join("obj/Release/Ghost.xaml"));
        write_nonempty(&source.join("bin/Release/OtherGhost.xaml"));
        let expected = expected_xbf_paths(&source).unwrap();
        assert!(!expected.iter().any(|path| path.starts_with("obj")));
        assert!(!expected.iter().any(|path| path.starts_with("bin")));
        verify_bundle(&app, &source).unwrap();
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn empty_or_wrong_source_tree_cannot_pass_vacuously() {
        let base = scratch("empty-source");
        let _ = fsx::force_remove_dir_all(&base);
        let source = base.join("source");
        fs::create_dir_all(&source).unwrap();
        assert!(expected_xbf_paths(&source).is_err());
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn managed_entry_assembly_is_in_the_signing_set() {
        assert!(FIRST_PARTY_PES.iter().any(|(source, stage)| {
            *source == "app/FindMyFiles.dll" && *stage == "app-FindMyFiles.dll"
        }));
    }

    #[test]
    fn developer_cli_is_not_an_end_user_bundle_or_signing_input() {
        assert!(!ENGINE_BINS.contains(&"fmf.exe"));
        assert!(!REQUIRED.contains(&"fmf.exe"));
        assert!(FIRST_PARTY_PES
            .iter()
            .all(|(source, _)| *source != "app/fmf.exe"));
    }

    #[test]
    fn shipped_readme_names_the_real_state_roots_and_full_uninstall_path() {
        for required in [
            "%ProgramData%\\find-my-files\\",
            "%APPDATA%\\find-my-files\\",
            "Manage service",
            "Remove service and all data...",
            "サービスの管理",
            "サービスとすべてのデータを削除…",
            "accept the one Windows administrator prompt",
            "both data directories",
            "上記2つのデータフォルダー",
            "scheduled task",
        ] {
            assert!(
                README.contains(required),
                "shipping README must retain accurate setup/uninstall detail: {required}"
            );
        }

        let lower = README.to_ascii_lowercase();
        for forbidden in [
            "data\\  next to this file",
            "folder is portable",
            "delete it, freely",
            "ポータブル構成",
            "削除も自由",
            "does not delete your per-user UI data",
            "ユーザー別 UI データを削除することはありません",
        ] {
            assert!(
                !lower.contains(&forbidden.to_ascii_lowercase()),
                "shipping README resurrected a false adjacent-data/portable claim: {forbidden}"
            );
        }
    }

    #[test]
    fn buildinfo_version_parser_requires_exactly_one_value() {
        assert_eq!(
            parse_buildinfo_version(
                "\u{feff}FindMyFiles\r\nversion:  0.1.1-dev+gabc1234\r\nchannel:  dev\r\n"
            )
            .unwrap(),
            "0.1.1-dev+gabc1234"
        );
        assert!(parse_buildinfo_version("FindMyFiles\nchannel: dev\n").is_err());
        assert!(parse_buildinfo_version("version:\n").is_err());
        assert!(parse_buildinfo_version("version: 0.1.1\nversion: 0.1.2\n").is_err());
    }

    #[test]
    fn launcher_and_buildinfo_must_share_one_identity() {
        validate_bundle_identity(
            "0.1.1-dev+gabc1234.dirty",
            "0.1.1-dev+gabc1234.dirty",
            "0.1.1",
            "0.1.1-dev+gabc1234.dirty",
        )
        .unwrap();
        assert!(validate_bundle_identity(
            "0.1.1-dev+gabc1234",
            "0.1.1-dev+gfffffff",
            "0.1.1",
            "0.1.1-dev+gabc1234",
        )
        .is_err());
        assert!(validate_bundle_identity(
            "0.1.1-dev+gabc1234",
            "0.1.1-dev+gabc1234",
            "0.1.0",
            "0.1.1-dev+gabc1234",
        )
        .is_err());
        assert!(validate_bundle_identity(
            "0.1.1-dev+gabc1234",
            "0.1.1-dev+gabc1234",
            "0.1.1",
            "0.1.0-dev+gabc1234",
        )
        .is_err());
    }
}
