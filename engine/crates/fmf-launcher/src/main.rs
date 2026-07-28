//! `fmf-launcher` — the tiny executable a user double-clicks at the root of the
//! distributable bundle.
//!
//! The real `WinUI` app and its ~100 self-contained runtime files live one level
//! down in `app\`: they cannot move, because the .NET apphost resolves its
//! managed DLLs, `*.deps.json` and `*.runtimeconfig.json` from its own
//! directory. This launcher sits alone at the top (beside only `README.txt`) so
//! "which file do I run" is obvious, then starts `app\FindMyFiles.exe`,
//! forwarding its own command-line arguments unchanged, and exits — the GUI
//! app outlives it, so only one process remains. Stable app state is never
//! redirected beside the executable: UI state belongs under `%APPDATA%` and
//! engine state under `%ProgramData%`.

#![windows_subsystem = "windows"]

use std::env;
use std::path::Path;
use std::process::Command;

/// Subdirectory holding the real self-contained app bundle.
const APP_SUBDIR: &str = "app";
/// The real `WinUI` apphost inside [`APP_SUBDIR`].
const APP_EXE: &str = "FindMyFiles.exe";
fn main() {
    let Some(dir) = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    else {
        fatal("Could not determine the launcher's own location.");
        return;
    };

    let app_exe = dir.join(APP_SUBDIR).join(APP_EXE);
    if !app_exe.exists() {
        fatal(&format!(
            "The application was not found at:\n{}\n\n\
             The download may be incomplete — re-extract the .zip, keeping its \
             folder structure intact.",
            app_exe.display()
        ));
        return;
    }

    let mut cmd = Command::new(&app_exe);
    cmd.current_dir(dir.join(APP_SUBDIR));
    cmd.args(env::args_os().skip(1));

    if let Err(e) = cmd.spawn() {
        fatal(&format!("Could not start the application:\n{e}"));
    }
    // Spawn-and-exit: do not wait. The detached GUI process keeps running after
    // this launcher returns (Windows does not reap children on parent exit).
}

/// Surface a fatal message to a GUI user. Under the `windows` subsystem there is
/// no console, so a message box is the only way to report the failure rather
/// than vanishing silently.
fn fatal(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let title: Vec<u16> = "FindMyFiles"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let body: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: both buffers are NUL-terminated UTF-16; a null owner HWND is valid
    // (a standalone, non-owned message box). The call has no other invariants.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
