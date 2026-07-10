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
//! `--data-dir` (in either the `=`-joined or the space-separated spelling), it
//! appends `--data-dir=<root>\data` so the index/settings/log tree lands beside
//! the launcher (the documented portable layout) instead of buried in `app\`.
//! Because the app itself only parses the `=`-joined form, a user-supplied
//! `--data-dir <value>` pair is rewritten into `--data-dir=<value>` before the
//! spawn so the space form is honoured too. A read-only location still falls
//! back to the per-user profile via the app's own `AppPaths` probe.

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
/// The bare (space-separated) spelling the launcher rewrites into [`DATA_DIR_FLAG`].
const DATA_DIR_FLAG_BARE: &str = "--data-dir";

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
    let (forwarded, has_data_dir) = normalize_data_dir(&forwarded);

    let mut cmd = Command::new(&app_exe);
    cmd.current_dir(dir.join(APP_SUBDIR));
    cmd.args(&forwarded);
    if !has_data_dir {
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

/// Normalize the forwarded arguments so the app sees a single `--data-dir=…`
/// spelling, and report whether the user supplied one at all (in which case the
/// launcher must not append its portable default).
///
/// The app only parses the `=`-joined form (ASCII case-insensitive), so a bare
/// `--data-dir <value>` pair — the natural spelling a user might type — is
/// rewritten in place into `--data-dir=<value>`. A trailing bare `--data-dir`
/// with no following value is not a usable choice: it is forwarded verbatim and
/// the default still applies. Any already-`=`-joined form is honoured as-is.
fn normalize_data_dir(args: &[OsString]) -> (Vec<OsString>, bool) {
    let mut out: Vec<OsString> = Vec::with_capacity(args.len());
    let mut present = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(s) = arg.to_str() {
            // Already `=`-joined (`--data-dir=…`): honour as-is.
            if s.get(..DATA_DIR_FLAG.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(DATA_DIR_FLAG))
            {
                present = true;
                out.push(arg.clone());
                i += 1;
                continue;
            }
            // Bare `--data-dir` with a following value: rewrite the pair. A
            // trailing bare `--data-dir` (no value) falls through, forwarded
            // verbatim so the launcher's default still applies.
            if s.eq_ignore_ascii_case(DATA_DIR_FLAG_BARE)
                && let Some(value) = args.get(i + 1)
            {
                let mut joined = OsString::from(DATA_DIR_FLAG);
                joined.push(value);
                out.push(joined);
                present = true;
                i += 2;
                continue;
            }
        }
        out.push(arg.clone());
        i += 1;
    }
    (out, present)
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
        let (out, present) = normalize_data_dir(&args);
        assert!(present);
        assert_eq!(out, args); // honoured verbatim
    }

    #[test]
    fn detects_user_data_dir_case_insensitive() {
        let args = [OsString::from("--Data-Dir=C:\\tmp")];
        let (out, present) = normalize_data_dir(&args);
        assert!(present);
        assert_eq!(out, args);
    }

    #[test]
    fn space_form_is_normalized_to_equals_form_and_honoured() {
        // The space-separated spelling must be honoured exactly like the
        // `=`-joined one: the pair is rewritten and no default is appended.
        let space = [OsString::from("--data-dir"), OsString::from("C:\\tmp")];
        let (out, present) = normalize_data_dir(&space);
        assert!(present);
        assert_eq!(out, [OsString::from("--data-dir=C:\\tmp")]);
        // Equivalence: same result as the user typing the `=`-joined form.
        let (equals_out, _) = normalize_data_dir(&[OsString::from("--data-dir=C:\\tmp")]);
        assert_eq!(out, equals_out);
    }

    #[test]
    fn space_form_case_insensitive_is_normalized() {
        let args = [OsString::from("--Data-Dir"), OsString::from("C:\\tmp")];
        let (out, present) = normalize_data_dir(&args);
        assert!(present);
        assert_eq!(out, [OsString::from("--data-dir=C:\\tmp")]);
    }

    #[test]
    fn space_form_preserves_surrounding_args_and_order() {
        let args = [
            OsString::from("--fake-engine"),
            OsString::from("--data-dir"),
            OsString::from("C:\\tmp"),
            OsString::from("!!warn"),
        ];
        let (out, present) = normalize_data_dir(&args);
        assert!(present);
        assert_eq!(
            out,
            [
                OsString::from("--fake-engine"),
                OsString::from("--data-dir=C:\\tmp"),
                OsString::from("!!warn"),
            ]
        );
    }

    #[test]
    fn trailing_bare_data_dir_falls_through_to_default() {
        // No value follows — not a usable choice, so it is forwarded verbatim
        // and the launcher's default still applies.
        let args = [OsString::from("--data-dir")];
        let (out, present) = normalize_data_dir(&args);
        assert!(!present);
        assert_eq!(out, args);
    }

    #[test]
    fn unrelated_flags_do_not_match() {
        let args = [OsString::from("--fake-engine"), OsString::from("!!warn")];
        let (out, present) = normalize_data_dir(&args);
        assert!(!present);
        assert_eq!(out, args);
    }

    #[test]
    fn empty_args_have_no_data_dir() {
        let (out, present) = normalize_data_dir(&[]);
        assert!(!present);
        assert!(out.is_empty());
    }
}
