//! `fmf-launcher` — the tiny executable a user double-clicks at the root of the
//! distributable bundle.
//!
//! The real `WinUI` app and its ~100 self-contained runtime files live one level
//! down in `app\`: they cannot move, because the .NET apphost resolves its
//! managed DLLs, `*.deps.json` and `*.runtimeconfig.json` from its own
//! directory. This launcher sits alone at the top (beside only `README.txt`) so
//! "which file do I run" is obvious, then starts `app\FindMyFiles.exe`,
//! forwarding its own command-line arguments, and exits — the GUI app outlives
//! it, so only one process remains.
//!
//! It also keeps portable state at the bundle root: when the user did not pass a
//! `--data-dir=…`, it appends `--data-dir=<root>\data` so the index/settings/log
//! tree lands beside the launcher (the documented portable layout) instead of
//! buried in `app\`. A read-only location still falls back to the per-user
//! profile via the app's own `AppPaths` probe.

#![windows_subsystem = "windows"]

use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Subdirectory holding the real self-contained app bundle.
const APP_SUBDIR: &str = "app";
/// The real `WinUI` apphost inside [`APP_SUBDIR`].
const APP_EXE: &str = "FindMyFiles.exe";
/// Portable-state directory created beside the launcher.
const DATA_SUBDIR: &str = "data";
/// The app's data-dir flag (it only honours the `=`-joined form, case-insensitively).
const DATA_DIR_FLAG: &str = "--data-dir=";

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

    let forwarded: Vec<OsString> = env::args_os().skip(1).collect();

    let mut cmd = Command::new(&app_exe);
    cmd.current_dir(dir.join(APP_SUBDIR));
    cmd.args(&forwarded);
    if !has_data_dir(&forwarded) {
        let mut arg = OsString::from(DATA_DIR_FLAG);
        arg.push(dir.join(DATA_SUBDIR));
        cmd.arg(arg);
    }

    if let Err(e) = cmd.spawn() {
        fatal(&format!("Could not start the application:\n{e}"));
    }
    // Spawn-and-exit: do not wait. The detached GUI process keeps running after
    // this launcher returns (Windows does not reap children on parent exit).
}

/// True when the user already passed a `--data-dir=…` the launcher must not
/// override. Matches the exact form the app honours (`=`-joined, ASCII
/// case-insensitive), so any other spelling falls through to the default.
fn has_data_dir(args: &[OsString]) -> bool {
    args.iter().filter_map(|a| a.to_str()).any(|s| {
        s.get(..DATA_DIR_FLAG.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(DATA_DIR_FLAG))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_user_data_dir_equals_form() {
        let args = [OsString::from("--data-dir=C:\\tmp")];
        assert!(has_data_dir(&args));
    }

    #[test]
    fn detects_user_data_dir_case_insensitive() {
        let args = [OsString::from("--Data-Dir=C:\\tmp")];
        assert!(has_data_dir(&args));
    }

    #[test]
    fn split_form_is_not_honoured_so_default_still_applies() {
        // The app ignores the space-separated spelling, so the launcher must NOT
        // treat it as "user supplied" — it should append its own default.
        let args = [OsString::from("--data-dir"), OsString::from("C:\\tmp")];
        assert!(!has_data_dir(&args));
    }

    #[test]
    fn unrelated_flags_do_not_match() {
        let args = [OsString::from("--fake-engine"), OsString::from("!!warn")];
        assert!(!has_data_dir(&args));
    }

    #[test]
    fn empty_args_have_no_data_dir() {
        assert!(!has_data_dir(&[]));
    }
}
