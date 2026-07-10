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

use crate::{cmd, fsx, locale, notices, paths, prune, version};
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
/// download everywhere else. Guarding here keeps the `SelfContained` regression
/// (see `FindMyFiles.csproj`) from ever shipping green again.
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
    prune_publish_artifacts(&app)?;
    copy_engine_bins(&app)?;
    verify_bundle(&app)?;
    place_launcher_and_readme(&dist)?;
    place_buildinfo(&dist)?;
    place_completions(&app, &dist)?;
    place_legal_notices(&root, &dist)?;

    println!(
        "publish: build/dist/FindMyFiles assembled and verified \
         (root launcher + LICENSE.txt + THIRD-PARTY-NOTICES.txt + app/ with {} \
         required files + shell completions).",
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
/// `--offline --locked`: the release/nightly `just publish` builds the engine
/// first, so the crate sources are already in the cargo cache — running offline
/// makes the output reproducible (no clearlydefined.io round-trip, no wall-clock
/// or service-availability variance) and keeps a release build from depending on
/// a third-party web service. `cargo-about` is provisioned via the `mise.toml`
/// pin (release/nightly use mise-action; the ci.yml `app` job installs it +
/// warms the cache alongside).
///
/// Output goes through `-o <file>`, not stdout: cargo-about refuses a redirected
/// stdout on Windows (PowerShell re-encodes piped bytes to UTF-16 and corrupts
/// the license texts), so it writes a UTF-8 file we read back.
fn generate_rust_notices() -> Result<String> {
    let engine = paths::engine_dir();
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

Licensing: FindMyFiles is Apache-2.0 — full text in  LICENSE.txt  (here). The
third-party code it bundles (.NET runtime, Windows App SDK, Rust crates, ...) is
attributed in  THIRD-PARTY-NOTICES.txt  (here).

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

ライセンス: FindMyFiles は Apache-2.0 です(全文はこのフォルダーの  LICENSE.txt )。
同梱する第三者コード(.NET ランタイム/Windows App SDK/Rust クレート ほか)の
帰属表示はこのフォルダーの  THIRD-PARTY-NOTICES.txt  にあります。

Apache-2.0  -  https://github.com/P4suta/find-my-files
";
