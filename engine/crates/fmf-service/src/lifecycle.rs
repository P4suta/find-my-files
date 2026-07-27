//! On-demand service lifecycle (ADR-0027): the machine-wide "last use" stamp,
//! the stable binary-copy location, and the pure idle-stop / GC decisions.
//!
//! The two decisions are pure functions over their inputs, unit-tested without
//! a running service — the same testable-seam discipline as the app-side
//! `DecideAuto`. All time/file I/O lives at the edges of this module.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Scheduled Task name for the daily GC (registered by `install`, removed by
/// `uninstall`/`gc`). A fixed constant — the task action is the stable binary
/// copy plus the `gc` verb, never user input.
pub const GC_TASK_NAME: &str = "find-my-files engine GC";

/// Seconds in a day — the GC threshold is expressed in days.
const SECS_PER_DAY: u64 = 86_400;

/// Path of the machine-wide `last_use` stamp.
///
/// `%ProgramData%\find-my-files\last_use` — Unix seconds (text) of the most
/// recent client connection / graceful stop, read by `gc` to age out an unused
/// install. Lives in the SYSTEM+Administrators data root, so a standard user
/// cannot forge it.
#[must_use]
pub fn last_use_path(data_dir: &Path) -> PathBuf {
    data_dir.join("last_use")
}

/// Path of the stable service-binary copy in the data root.
///
/// `%ProgramData%\find-my-files\fmf-service.exe` — the service binary copied out
/// of the (portable) app bundle at install, so the SCM registration and the GC
/// task survive the app folder being deleted, and so a standard user — who
/// cannot write the hardened data root — cannot replace the SYSTEM binary.
#[must_use]
pub fn stable_exe_path(data_dir: &Path) -> PathBuf {
    data_dir.join("fmf-service.exe")
}

/// Atomically records "the service was used now" through the already verified
/// machine-root handle. No path-only staging or replacement API is used.
///
/// # Errors
///
/// Returns timestamp conversion or trusted-root publication errors. Callers
/// must surface the failure rather than letting GC mistake an old stamp for an
/// unused installation.
pub fn stamp_last_use(root: &crate::security::TrustedDataRoot) -> std::io::Result<()> {
    stamp_last_use_with_sddl(root, SystemTime::now(), &crate::security::data_dir_sddl())
}

fn stamp_last_use_with_sddl(
    root: &crate::security::TrustedDataRoot,
    used_at: SystemTime,
    sddl: &str,
) -> std::io::Result<()> {
    let secs = used_at
        .duration_since(UNIX_EPOCH)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?
        .as_secs();
    root.atomic_write("last_use", secs.to_string().as_bytes(), sddl)
}

/// Reads the last-use stamp.
///
/// A missing stamp is `Ok(None)`. Malformed content and every other I/O failure
/// are errors so the GC command fails closed instead of silently classifying a
/// damaged installation as fresh or stale.
///
/// # Errors
///
/// Returns non-`NotFound` I/O errors, `InvalidData` for malformed Unix seconds,
/// or `InvalidData` when the timestamp cannot be represented by `SystemTime`.
pub fn read_last_use(data_dir: &Path) -> std::io::Result<Option<SystemTime>> {
    let path = last_use_path(data_dir);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    read_last_use_file(file, &path).map(Some)
}

/// Reads a last-use stamp from an already verified file handle.
///
/// # Errors
/// Returns I/O errors, `InvalidData` for malformed/oversized Unix seconds, or
/// `InvalidData` when the timestamp cannot be represented by `SystemTime`.
pub fn read_last_use_file(
    mut file: std::fs::File,
    display_path: &Path,
) -> std::io::Result<SystemTime> {
    use std::io::Read;

    const MAX_LAST_USE_BYTES: u64 = 32;
    let mut text = String::new();
    file.by_ref()
        .take(MAX_LAST_USE_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_LAST_USE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} exceeds {MAX_LAST_USE_BYTES} bytes",
                display_path.display()
            ),
        ));
    }
    let secs = text.trim().parse::<u64>().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("malformed {}: {e}", display_path.display()),
        )
    })?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("timestamp in {} is out of range", display_path.display()),
            )
        })
}

/// Pure idle-stop decision (ADR-0027).
///
/// Stop only once a client has connected and gone (`seen_client`), nothing is
/// live now (`active == 0`), no index pass is in flight (`indexing`), and the
/// idle gap has reached the timeout. `timeout == 0` (disabled) is handled by the
/// caller.
#[must_use]
pub fn idle_should_stop(
    seen_client: bool,
    active: usize,
    indexing: bool,
    idle_for: Duration,
    timeout: Duration,
) -> bool {
    seen_client && active == 0 && !indexing && idle_for >= timeout
}

/// Serializes the daily GC Scheduled Task definition for `schtasks /Create /XML`.
///
/// Returned as **UTF-16LE with a BOM** under an `encoding="UTF-16"` declaration:
/// `schtasks` starts reading the file as UTF-16 and aborts at the declaration
/// with "Cannot switch the encoding" `(1,40)` on non-English Windows (e.g. ja-JP)
/// when the bytes are UTF-8. UTF-16LE+BOM is the form Windows itself exports, so
/// the definition loads on every locale. `<Command>`/`<Arguments>` are separate
/// elements, sidestepping `/TR` command-line quoting; the action runs the stable
/// binary copy with the `gc` verb as SYSTEM (`S-1-5-18`). The fixed descriptor
/// from [`crate::security::gc_task_sddl`] is embedded in registration metadata
/// so owner/group/DACL hardening is atomic with task creation. The `stable_exe`
/// process token is reduced to `SeChangeNotifyPrivilege`, matching the service.
/// The `stable_exe` path is the fixed hardened-data-root copy (never user input),
/// so it needs no escaping.
#[must_use]
pub fn gc_task_xml(stable_exe: &Path) -> Vec<u8> {
    let command = xml_text(&stable_exe.display().to_string());
    let security_descriptor = xml_text(crate::security::gc_task_sddl());
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.3\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         <RegistrationInfo><SecurityDescriptor>{security_descriptor}</SecurityDescriptor><Description>find-my-files engine on-demand GC (ADR-0027)</Description></RegistrationInfo>\n\
         <Triggers><CalendarTrigger><StartBoundary>2024-01-01T03:00:00</StartBoundary><Enabled>true</Enabled><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger></Triggers>\n\
         <Principals><Principal id=\"Author\"><UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel><RequiredPrivileges><Privilege>SeChangeNotifyPrivilege</Privilege></RequiredPrivileges></Principal></Principals>\n\
         <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><StartWhenAvailable>true</StartWhenAvailable><Enabled>true</Enabled><ExecutionTimeLimit>PT5M</ExecutionTimeLimit></Settings>\n\
         <Actions Context=\"Author\"><Exec><Command>{command}</Command><Arguments>gc</Arguments></Exec></Actions>\n\
         </Task>\n",
    );
    // UTF-16LE + BOM (see the doc comment): the BOM is what tells schtasks to read
    // the rest as UTF-16, matching the declaration.
    let mut bytes = Vec::with_capacity(2 + xml.len() * 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]); // UTF-16LE byte-order mark
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Pure GC decision (ADR-0027): remove an install unused for `max_idle_days`.
///
/// `0` disables it; a missing (`None`) stamp is conservative — never GC without
/// evidence of staleness; a `last_use` in the future (clock skew) never fires.
#[must_use]
pub fn gc_should_remove(now: SystemTime, last_use: Option<SystemTime>, max_idle_days: u64) -> bool {
    if max_idle_days == 0 {
        return false;
    }
    let Some(last) = last_use else { return false };
    let Ok(idle) = now.duration_since(last) else {
        return false; // last_use is in the future — clock skew, do nothing
    };
    idle >= Duration::from_secs(max_idle_days.saturating_mul(SECS_PER_DAY))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> (tempfile::TempDir, crate::security::TrustedDataRoot, String) {
        let anchor = tempfile::tempdir().expect("anchor");
        let sid = crate::security::current_user_sid().expect("current SID");
        let sddl = format!("O:{sid}G:BUD:P(A;OICI;FA;;;{sid})");
        let root = crate::security::TrustedDataRoot::create_or_replace_for_test(
            &anchor.path().join("find-my-files"),
            &sddl,
        )
        .expect("trusted test root");
        (anchor, root, sddl)
    }

    #[test]
    fn idle_stop_requires_seen_idle_and_not_indexing() {
        let t = Duration::from_mins(5);
        // Happy path: a client came and went, nothing live, not indexing, gap reached.
        assert!(idle_should_stop(
            true,
            0,
            false,
            Duration::from_secs(301),
            t
        ));
        // Never saw a client → don't stop (client-less console bring-up).
        assert!(!idle_should_stop(
            false,
            0,
            false,
            Duration::from_secs(999),
            t
        ));
        // A live connection → never stop.
        assert!(!idle_should_stop(
            true,
            1,
            false,
            Duration::from_secs(999),
            t
        ));
        // An index pass in flight → never stop.
        assert!(!idle_should_stop(
            true,
            0,
            true,
            Duration::from_secs(999),
            t
        ));
        // Gap not yet reached → keep waiting.
        assert!(!idle_should_stop(true, 0, false, Duration::from_mins(2), t));
    }

    #[test]
    fn gc_ages_out_only_a_stale_stamp() {
        let now = UNIX_EPOCH + Duration::from_secs(30 * SECS_PER_DAY);
        let eight_days_ago = UNIX_EPOCH + Duration::from_secs(22 * SECS_PER_DAY);
        let yesterday = UNIX_EPOCH + Duration::from_secs(29 * SECS_PER_DAY);
        // 8 days idle, threshold 7 → remove.
        assert!(gc_should_remove(now, Some(eight_days_ago), 7));
        // 1 day idle → keep.
        assert!(!gc_should_remove(now, Some(yesterday), 7));
        // Disabled (0) → never remove, even when ancient.
        assert!(!gc_should_remove(now, Some(eight_days_ago), 0));
        // No stamp → conservative keep.
        assert!(!gc_should_remove(now, None, 7));
        // Future stamp (clock skew) → keep.
        let future = UNIX_EPOCH + Duration::from_secs(40 * SECS_PER_DAY);
        assert!(!gc_should_remove(now, Some(future), 7));
    }

    #[test]
    fn gc_task_xml_is_utf16le_bom_for_schtasks() {
        let exe = Path::new(r"C:\ProgramData\find-my-files\fmf-service.exe");
        let bytes = gc_task_xml(exe);
        // Regression (ja-JP): it was UTF-8 and `schtasks /Create /XML` failed with
        // "Cannot switch the encoding" at the declaration. Must be UTF-16LE + BOM.
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "missing UTF-16LE BOM");
        assert_eq!(bytes.len() % 2, 0, "UTF-16 code units are 2 bytes");
        // Decode back past the BOM and check the declaration + the SYSTEM action.
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16(&units).expect("round-trips as UTF-16");
        assert!(
            text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"),
            "declaration must announce UTF-16"
        );
        assert!(
            text.contains("<Command>C:\\ProgramData\\find-my-files\\fmf-service.exe</Command>"),
            "action runs the stable exe"
        );
        assert!(
            text.contains("<Arguments>gc</Arguments>"),
            "with the gc verb"
        );
        assert!(text.contains("<UserId>S-1-5-18</UserId>"), "as SYSTEM");
        assert!(
            text.contains(
                "<RequiredPrivileges><Privilege>SeChangeNotifyPrivilege</Privilege></RequiredPrivileges>"
            ),
            "the SYSTEM GC process receives only its traversal privilege"
        );
        assert!(
            text.contains(
                "<SecurityDescriptor>O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)</SecurityDescriptor>"
            ),
            "task owner/group/DACL are fixed at registration"
        );
    }

    #[test]
    fn gc_task_xml_escapes_the_command_as_xml_text() {
        let bytes = gc_task_xml(Path::new(r"C:\ProgramData\A&B\fmf-service.exe"));
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16(&units).expect("UTF-16");
        assert!(text.contains(r"<Command>C:\ProgramData\A&amp;B\fmf-service.exe</Command>"));
        assert!(!text.contains(r"<Command>C:\ProgramData\A&B"));
    }

    #[test]
    fn last_use_round_trips() {
        let (_anchor, root, sddl) = test_root();
        assert!(
            read_last_use(root.path()).expect("read missing").is_none(),
            "no stamp yet"
        );
        stamp_last_use_with_sddl(&root, SystemTime::now(), &sddl).expect("stamp");
        let t = read_last_use(root.path())
            .expect("read stamp")
            .expect("stamp then read");
        let age = SystemTime::now()
            .duration_since(t)
            .expect("stamp is not in the future");
        assert!(age < Duration::from_mins(1), "stamp is ~now");
        assert!(
            std::fs::read_dir(root.path())
                .expect("read data dir")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".fmf-stage-")),
            "successful publication leaves no staging file"
        );
    }

    #[test]
    fn last_use_atomically_replaces_an_existing_stamp() {
        let (_anchor, root, sddl) = test_root();
        root.atomic_write("last_use", b"1", &sddl)
            .expect("old stamp");
        let expected = UNIX_EPOCH + Duration::from_secs(123_456);

        stamp_last_use_with_sddl(&root, expected, &sddl).expect("replace stamp");

        assert_eq!(
            read_last_use(root.path()).expect("read"),
            Some(expected),
            "the complete replacement is visible"
        );
    }

    #[test]
    fn last_use_rejects_corruption_instead_of_treating_it_as_missing() {
        let dir = fmf_core::index::testutil::TestDir::new();
        std::fs::write(last_use_path(dir.path()), b"not-unix-seconds").expect("corrupt stamp");

        let error = read_last_use(dir.path()).expect_err("corruption must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn last_use_rejects_oversized_control_file() {
        let dir = fmf_core::index::testutil::TestDir::new();
        std::fs::write(last_use_path(dir.path()), vec![b'1'; 33]).expect("oversized stamp");

        let error = read_last_use(dir.path()).expect_err("control file read must be bounded");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn last_use_propagates_non_not_found_io_errors() {
        let dir = fmf_core::index::testutil::TestDir::new();
        std::fs::create_dir(last_use_path(dir.path())).expect("directory at stamp path");

        let error = read_last_use(dir.path()).expect_err("I/O faults must fail closed");

        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "only a genuinely missing stamp is Ok(None)"
        );
    }

    #[test]
    fn failed_last_use_replace_cleans_its_staging_file() {
        let (_anchor, root, sddl) = test_root();
        root.ensure_directory("last_use", &sddl)
            .expect("blocking destination");

        stamp_last_use_with_sddl(&root, UNIX_EPOCH + Duration::from_secs(7), &sddl)
            .expect_err("a directory cannot be replaced by the stamp file");

        assert!(
            std::fs::read_dir(root.path())
                .expect("read data dir")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".fmf-stage-")),
            "failed publication cleans its staging file"
        );
    }
}
