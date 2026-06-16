//! Scope-mode change source (ADR-0024), the `JournalSource` second
//! implementation that pairs with the folder-walk scanner.
//!
//! **Phase 1 (this file): a no-op journal.** The walk builds the index once
//! and it stays static until the app restarts (the user's manual re-index).
//! `read_blocking` never reports a change — it parks briefly and returns a
//! benign empty batch so the worker can re-check its stop flag, exactly like
//! the idle path of the USN journal. The walk snapshot is stamped with
//! `journal_id == 0`, which `snapshot_decision` always restores (no USN
//! cursor to validate), so a restart reloads the snapshot rather than
//! re-walking.
//!
//! **Phase 2 (planned):** replace `read_blocking` with one overlapped
//! `ReadDirectoryChangesW` handle per root behind a single IOCP, translating
//! `FILE_NOTIFY_INFORMATION` into synthesized `UsnRecord`s (path → synthetic
//! FRN via `scan::walk_id`), with buffer-overflow → re-walk and a periodic
//! re-walk fallback for network/cloud roots.

use std::time::Duration;

use crate::usn::{ReadOutcome, StatFetcher, UsnError};

use super::seams::{JournalSource, JournalView};

/// Synthetic journal identity for a walk snapshot. `0` makes
/// `snapshot_decision` always restore a loaded walk snapshot (there is no USN
/// retention window to fall outside of).
const WALK_JOURNAL_ID: u64 = 0;

/// How long the no-op `read_blocking` parks between benign wakeups. Bounds
/// shutdown latency (the worker re-checks `stop` on each wakeup) at the cost
/// of one cheap timer per scope slot; Phase 2 replaces the park with an IOCP
/// wait that returns the instant a watched root changes.
const IDLE_PARK: Duration = Duration::from_millis(250);

/// Non-elevated change source for scope mode. Holds the configured roots for
/// Phase 2's watchers; Phase 1 keeps them only to log/identify the session.
pub(super) struct WatcherJournalSource {
    #[allow(dead_code)] // Phase 2: one ReadDirectoryChangesW handle per root.
    roots: Vec<String>,
}

impl WatcherJournalSource {
    pub(super) const fn new(roots: Vec<String>) -> Self {
        Self { roots }
    }
}

impl JournalSource for WatcherJournalSource {
    fn open(&mut self) -> Result<(), UsnError> {
        Ok(())
    }

    fn query(&mut self) -> Result<JournalView, UsnError> {
        Ok(JournalView::scope())
    }

    fn read_blocking(&mut self, _buf: &mut Vec<u8>) -> Result<ReadOutcome, UsnError> {
        // Phase 1: the index is static. A periodic empty batch is the benign
        // wakeup the worker treats as "nothing to apply, re-check stop".
        std::thread::sleep(IDLE_PARK);
        Ok(ReadOutcome::Records {
            records: Vec::new(),
            truncated: false,
        })
    }

    fn journal_id(&self) -> u64 {
        WALK_JOURNAL_ID
    }

    fn next_usn(&self) -> i64 {
        0
    }

    fn set_next_usn(&mut self, _usn: i64) {}

    fn open_stat_fetcher(&self) -> Result<Box<dyn StatFetcher>, UsnError> {
        // Never consulted in Phase 1 (no records are ever applied); Phase 2's
        // watcher pre-stats changed paths into a per-batch map instead.
        Ok(Box::new(NullStatFetcher))
    }
}

/// A stat fetcher that knows nothing — apply never calls it in Phase 1.
struct NullStatFetcher;

impl StatFetcher for NullStatFetcher {
    fn stat(&self, _frn: u64) -> Option<(u64, i64)> {
        None
    }
}
