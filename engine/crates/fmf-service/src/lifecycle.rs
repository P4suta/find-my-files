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

/// Records "the service was used now" (best-effort). A write failure is a warn,
/// never fatal — the service must keep serving.
pub fn stamp_last_use(data_dir: &Path) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = last_use_path(data_dir);
    if let Err(e) = std::fs::write(&path, secs.to_string()) {
        tracing::warn!(path = %path.display(), error = %e, "last_use stamp failed");
    }
}

/// Reads the last-use stamp, or `None` when it is missing/unparsable (a fresh
/// install that has never served, or a corrupt byte).
#[must_use]
pub fn read_last_use(data_dir: &Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(last_use_path(data_dir)).ok()?;
    let secs: u64 = text.trim().parse().ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
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
/// binary copy with the `gc` verb as SYSTEM (`S-1-5-18`). The `stable_exe` path is
/// the fixed hardened-data-root copy (never user input), so it needs no escaping.
#[must_use]
pub fn gc_task_xml(stable_exe: &Path) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         <RegistrationInfo><Description>find-my-files engine on-demand GC (ADR-0027)</Description></RegistrationInfo>\n\
         <Triggers><CalendarTrigger><StartBoundary>2024-01-01T03:00:00</StartBoundary><Enabled>true</Enabled><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger></Triggers>\n\
         <Principals><Principal id=\"Author\"><UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel></Principal></Principals>\n\
         <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><StartWhenAvailable>true</StartWhenAvailable><Enabled>true</Enabled><ExecutionTimeLimit>PT5M</ExecutionTimeLimit></Settings>\n\
         <Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>gc</Arguments></Exec></Actions>\n\
         </Task>\n",
        stable_exe.display()
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
    }

    #[test]
    fn last_use_round_trips() {
        let dir = fmf_core::index::testutil::TestDir::new();
        assert!(read_last_use(dir.path()).is_none(), "no stamp yet");
        stamp_last_use(dir.path());
        let t = read_last_use(dir.path()).expect("stamp then read");
        let age = SystemTime::now()
            .duration_since(t)
            .expect("stamp is not in the future");
        assert!(age < Duration::from_mins(1), "stamp is ~now");
    }
}
