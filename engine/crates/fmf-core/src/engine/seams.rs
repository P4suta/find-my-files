//! The engine's only two OS-effect seams (ADR-0018 — this is the hard cap,
//! do not add a third): snapshot persistence and the USN journal session.
//! They exist so the volume worker's failure paths (corrupt snapshot,
//! journal-gone, failed saves, stat-fetch storms) replay in unprivileged,
//! deterministic tests (`worker_tests.rs`). The Windows implementations are
//! thin wrappers over the exact calls the worker made before the seam was
//! introduced — behavior-identical by construction.
//!
//! Granularity guard: every method here runs at establish/batch/save
//! frequency. Nothing per-entry goes through these traits (the per-record
//! `StatFetcher` handed out below was already a `dyn` call before the seam
//! existed — see `usn::apply_batch`).

use std::path::PathBuf;

use crate::index::VolumeIndex;
#[cfg(windows)]
use crate::usn::{ReadOutcome, StatFetcher, UsnError, UsnJournal, VolumeStatFetcher};

/// Snapshot persistence for one volume (`{index_dir}\{letter}.fmfidx`).
pub trait SnapshotStore: Send + Sync {
    /// Load the persisted snapshot: the rebuilt index plus the journal
    /// checkpoint (`journal_id`, `next_usn`) it was saved with.
    /// `ErrorKind::NotFound` means "first run" (not a failure); any other
    /// error means a corrupt/unreadable file — the caller counts it in
    /// `snapshot_load_failures` and falls back to a full scan.
    fn load(&self) -> std::io::Result<(VolumeIndex, u64, i64)>;
    /// Size of the persisted snapshot in bytes (observability only — the
    /// restore `ScanTrace`). Best effort; 0 when unknown.
    fn file_bytes(&self) -> u64;
    /// Persist the index with its checkpoint atomically (tmp + rename):
    /// a torn write must never become a loadable snapshot.
    fn save_atomic(&self, idx: &VolumeIndex, journal_id: u64, next_usn: i64)
    -> std::io::Result<()>;
    /// Best-effort removal (journal-gone: a snapshot pinned to the dead
    /// journal id must not be restored on the next start). Failures are
    /// intentionally ignored — checkpoint validation on load rejects a
    /// stale file anyway.
    fn remove(&self);
}

/// Production store: thin wrapper over `VolumeIndex::{load_from, save_to}`
/// plus `fs::{metadata, remove_file}` on the volume's snapshot path.
pub struct WinSnapshotStore {
    path: PathBuf,
}

impl WinSnapshotStore {
    pub(crate) const fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl SnapshotStore for WinSnapshotStore {
    fn load(&self) -> std::io::Result<(VolumeIndex, u64, i64)> {
        VolumeIndex::load_from(&self.path)
    }

    fn file_bytes(&self) -> u64 {
        std::fs::metadata(&self.path).map_or(0, |m| m.len())
    }

    fn save_atomic(
        &self,
        idx: &VolumeIndex,
        journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        idx.save_to(&self.path, journal_id, next_usn)
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What checkpoint validation needs from `FSCTL_QUERY_USN_JOURNAL`.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalView {
    pub(crate) journal_id: u64,
    /// Oldest USN still retained — a persisted cursor older than this has
    /// lost records and cannot be replayed.
    pub(crate) first_usn: i64,
}

/// One volume's USN journal session, reopenable across journal-gone
/// rescans. `open` must succeed before any other method is called (the
/// worker guarantees this; implementations may panic otherwise — the
/// worker's panic firewall turns that into a visible `VolumeFailed`).
#[cfg(windows)]
pub trait JournalSource: Send {
    /// (Re)open the journal, creating it when missing. Positions the
    /// cursor at the journal's current end. Called once per establish
    /// cycle: at start and after every journal-gone rescan.
    fn open(&mut self) -> Result<(), UsnError>;
    /// Live journal identity/retention for checkpoint validation.
    fn query(&mut self) -> Result<JournalView, UsnError>;
    /// Blocking read of the next batch. Semantics the worker relies on:
    /// blocks until records exist, then returns them and advances the
    /// cursor past the batch; `Gone` when the journal died under us; `Err`
    /// on an unrecoverable failure. An empty `Records` batch is a benign
    /// wakeup — the worker re-checks its stop flag and reads again (fakes
    /// use this to unblock on stop; the live read returns on the next
    /// volume write, which is what keeps `Engine::shutdown`'s join prompt).
    fn read_blocking(&mut self, buf: &mut Vec<u8>) -> Result<ReadOutcome, UsnError>;
    /// Journal id of the open session (checkpoint identity).
    fn journal_id(&self) -> u64;
    /// Cursor the next read starts at (the value persisted in checkpoints).
    fn next_usn(&self) -> i64;
    /// Reposition the cursor (a snapshot restore replays from its
    /// persisted checkpoint instead of the journal's current end).
    fn set_next_usn(&mut self, usn: i64);
    /// Size/mtime fetcher bound to the same volume, opened once per tail
    /// session. Per-record `stat` calls were already dynamic (`usn::apply`).
    fn open_stat_fetcher(&self) -> Result<Box<dyn StatFetcher>, UsnError>;
}

/// Production journal: thin wrapper over `usn::session::UsnJournal` /
/// `VolumeStatFetcher` for one drive label.
#[cfg(windows)]
pub struct WinJournalSource {
    label: String,
    session: Option<UsnJournal>,
}

#[cfg(windows)]
impl WinJournalSource {
    pub(crate) const fn new(label: String) -> Self {
        Self {
            label,
            session: None,
        }
    }

    const fn session(&self) -> &UsnJournal {
        self.session.as_ref().expect("journal used before open")
    }
}

#[cfg(windows)]
impl JournalSource for WinJournalSource {
    fn open(&mut self) -> Result<(), UsnError> {
        // Drop the previous session first — the pre-seam code's rebinding
        // per outer-loop iteration closed the old volume handle before
        // opening the new one.
        self.session = None;
        self.session = Some(UsnJournal::open(&self.label, None)?);
        Ok(())
    }

    fn query(&mut self) -> Result<JournalView, UsnError> {
        self.session().query().map(|d| JournalView {
            journal_id: d.UsnJournalID,
            first_usn: d.FirstUsn,
        })
    }

    fn read_blocking(&mut self, buf: &mut Vec<u8>) -> Result<ReadOutcome, UsnError> {
        self.session
            .as_mut()
            .expect("journal used before open")
            .read_blocking(buf)
    }

    fn journal_id(&self) -> u64 {
        self.session().journal_id
    }

    fn next_usn(&self) -> i64 {
        self.session().next_usn
    }

    fn set_next_usn(&mut self, usn: i64) {
        self.session
            .as_mut()
            .expect("journal used before open")
            .next_usn = usn;
    }

    fn open_stat_fetcher(&self) -> Result<Box<dyn StatFetcher>, UsnError> {
        Ok(Box::new(VolumeStatFetcher::open(&self.label)?))
    }
}
