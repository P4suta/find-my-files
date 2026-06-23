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
