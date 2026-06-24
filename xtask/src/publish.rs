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

use crate::{cmd, fsx, locale, paths};
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
const REQUIRED: &[&str] = &[
    "FindMyFiles.exe",
    "WinRT.Runtime.dll",
    "fmf_engine.dll",
    "fmf-service.exe",
    "fmf.exe",
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

    println!(
        "publish: build/dist/FindMyFiles assembled and verified \
         (root launcher + app/ with {} required files).",
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

Apache-2.0  -  https://github.com/P4suta/find-my-files
";
