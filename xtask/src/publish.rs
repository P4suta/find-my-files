//! `xtask publish [--skip-rust]` — assemble the distributable bundle in
//! build/dist/FindMyFiles.
//!
//! Publishes the app (not a bare `dotnet build` — only the publish output wires
//! WinRT.Runtime.dll, the `WinAppSDK` native helpers and the compiled XAML into a
//! runnable bundle), prunes the locale dirs the app doesn't ship, copies the
//! engine binaries, then SELF-VERIFIES the result. The self-check is what lets
//! us drop ci.yml's separate "verify bundle is runnable" step: the producer of
//! the bundle guarantees its own output instead of a downstream guard.
//!
//! `--skip-rust true` skips the in-build cargo step (CI prebuilds + downloads
//! the engine binaries into build/engine/release/ before this runs).

use crate::{cmd, fsx, locale, paths, version};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

/// Engine binaries copied in alongside the published app.
const ENGINE_BINS: &[&str] = &["fmf-service.exe", "fmf.exe"];

/// Files whose presence (inside `app/`) means the bundle can actually launch.
/// `FindMyFiles.exe` (the apphost) + `WinRT.Runtime.dll` come from `dotnet
/// publish`, `fmf_engine.dll` via the csproj `<None Include>`, and the two
/// engine exes are copied below. The root-level launcher is verified separately
/// (it is what the user double-clicks; the apphost is its target).
///
/// `coreclr.dll` / `hostfxr.dll` are the proof the .NET runtime is actually
/// bundled (self-contained). `WinRT.Runtime.dll` alone is NOT enough — it also
/// ships in a framework-dependent build — so without these the bundle would
/// launch only where a matching .NET is already installed and demand a runtime
/// download everywhere else. Guarding here keeps the SelfContained regression
/// (see FindMyFiles.csproj) from ever shipping green again.
const REQUIRED: &[&str] = &[
    "FindMyFiles.exe",
    "WinRT.Runtime.dll",
    "coreclr.dll",
    "hostfxr.dll",
    "fmf_engine.dll",
    "fmf-service.exe",
    "fmf.exe",
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
    ("app/fmf.exe", "fmf.exe"),
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

pub fn run(skip_rust: bool) -> Result<()> {
    let root = paths::repo_root();
    let dist = paths::dist_dir();
    let app = paths::app_dir();

    // Clean the whole stale bundle (launcher + README + app/ payload). Best-
    // effort by design: a leftover bundle can be locked by a running app, and
    // `dotnet publish` overwrites anyway — the self-verify at the end is the
    // real gate. We warn rather than fail (the old recipe swallowed this).
    if let Err(e) = fsx::force_remove_dir_all(&dist) {
        eprintln!(
            "warning: could not fully clean {} ({e}); publishing over the leftovers",
            dist.display()
        );
    }

    // Publish the self-contained app into the `app/` subfolder — the bundle root
    // is reserved for the launcher + README so "which exe do I run" is obvious.
    // Pass the absolute path so the output location is the single source in
    // paths::app_dir(), independent of `cmd::run`'s working directory.
    let app_arg = app.to_str().context("app path is not valid UTF-8")?;
    let skip_arg = format!("-p:SkipRustBuild={skip_rust}");
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
    ];
    // In CI, build the shipped bundle from the pinned dependency graph: fail the
    // implicit restore if packages.lock.json is stale (reproducible supply
    // chain). Locally we stay lenient — a mid-edit dependency bump shouldn't
    // block `just publish`. The MSBuild property reaches the restore that
    // `dotnet publish` runs (more robust than the `--locked-mode` CLI flag,
    // which `publish` does not forward in every SDK).
    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        args.push("-p:RestoreLockedMode=true");
    }
    cmd::run(&root, "dotnet", &args)?;

    prune_locales(&app)?;
    copy_engine_bins(&app)?;
    verify_bundle(&app)?;
    place_launcher_and_readme(&dist)?;
    place_buildinfo(&dist)?;
    place_completions(&app, &dist)?;

    println!(
        "publish: build/dist/FindMyFiles assembled and verified \
         (root launcher + app/ with {} required files + shell completions).",
        REQUIRED.len()
    );
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

fn verify_bundle(app: &Path) -> Result<()> {
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|f| !app.join(f).exists())
        .collect();
    if !missing.is_empty() {
        bail!(
            "bundle at {} is missing {missing:?} — it would not launch",
            app.display()
        );
    }
    Ok(())
}

/// Ship shell-completion scripts under the bundle's `completions/` dir, generated
/// by invoking the just-built `app/fmf.exe completions <shell>` — the scripts are
/// produced by the exact binary in the bundle, so they can never drift from it.
/// (Resolves the doc-vs-impl gap where completions were claimed to be bundled but
/// nothing copied them in.)
fn place_completions(app: &Path, dist: &Path) -> Result<()> {
    // (shell argument, output filename) — filenames follow clap_complete's own
    // conventions so an installed script is named what each shell expects.
    const SHELLS: &[(&str, &str)] = &[
        ("bash", "fmf.bash"),
        ("zsh", "_fmf"),
        ("fish", "fmf.fish"),
        ("powershell", "_fmf.ps1"),
    ];
    let fmf = app.join("fmf.exe");
    let out_dir = dist.join("completions");
    fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    for (shell, filename) in SHELLS {
        let output = std::process::Command::new(&fmf)
            .args(["completions", shell])
            .output()
            .with_context(|| format!("run {} completions {shell}", fmf.display()))?;
        if !output.status.success() {
            bail!("`fmf completions {shell}` exited with {}", output.status);
        }
        fs::write(out_dir.join(filename), &output.stdout)
            .with_context(|| format!("write completion {filename}"))?;
    }
    Ok(())
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
/// label uses the SAME precedence as the shipped binaries (`FMF_BUILD_VERSION`,
/// else the local `-dev+g<sha>` default), so the file never disagrees with what
/// `fmf --version` reports.
fn place_buildinfo(dist: &Path) -> Result<()> {
    let full = version::resolve_bundle_version()?;
    let commit_date = version::git_commit_date();
    let body = version::render_buildinfo(&full, commit_date.as_deref());
    // Same Notepad-friendly encoding as README.txt: UTF-8 BOM + CRLF.
    let text = format!("\u{feff}{}", body.replace('\n', "\r\n"));
    fs::write(dist.join("BUILDINFO.txt"), text).context("write BUILDINFO.txt")?;
    Ok(())
}

/// End-user README dropped at the bundle root, beside the launcher (English then
/// Japanese — the app ships both locales). Stored as LF; written as CRLF + BOM.
const README: &str = "\
FindMyFiles — fast filename search for Windows
==============================================

>> To start: double-click  FindMyFiles.exe  (here, in this folder).

That's it. The app and all its runtime files live in the  app\\  subfolder;
your index, settings and logs go in  data\\  next to this file — so the whole
folder is portable: copy it anywhere, or delete it, freely.

Faster whole-drive search (optional): the first time you ask for it, the app
can install a small background service (one Windows permission prompt). Until
then it searches the folders you choose.

Advanced tools, inside  app\\ :
  app\\fmf.exe          command-line search
  app\\fmf-service.exe  the background service (the app installs/manages it)

Shell completions for the  fmf  command are in  completions\\  (PowerShell, bash,
zsh, fish). For PowerShell, add to your profile:
  . \"$PWD\\completions\\_fmf.ps1\"
Or generate fresh at any time:  app\\fmf.exe completions powershell

Apache-2.0  -  https://github.com/P4suta/find-my-files

--------------------------------------------------------------------------

FindMyFiles — Windows 向け 高速ファイル名検索
==============================================

>> 起動: このフォルダーの  FindMyFiles.exe  をダブルクリック。

これだけです。アプリ本体と実行ファイル群は  app\\  サブフォルダーにあります。
索引・設定・ログはこのファイルの隣の  data\\  に保存されるので、フォルダーごと
どこへでもコピーでき、削除も自由なポータブル構成です。

全ドライブの最速検索(任意): 初回に頼むと、小さなバックグラウンドサービスを
導入できます(Windows の許可ダイアログが1回)。それまでは選んだフォルダーを
検索します。

上級者向けツールは  app\\  内:
  app\\fmf.exe          コマンドライン検索
  app\\fmf-service.exe  バックグラウンドサービス(アプリが導入・管理)

fmf コマンドの補完スクリプトは  completions\\  にあります(PowerShell/bash/zsh/fish)。
PowerShell ならプロファイルに次を追加:
  . \"$PWD\\completions\\_fmf.ps1\"
いつでも生成し直せます:  app\\fmf.exe completions powershell

Apache-2.0  -  https://github.com/P4suta/find-my-files
";
