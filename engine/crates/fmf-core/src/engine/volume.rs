//! Per-volume state: the slot the engine and the worker thread share
//! (`VolumeSlot`), the index install rule, the journal checkpoint, and the
//! snapshot save helper. The thread that drives the flow lives in
//! `worker.rs`; the OS-effect seams it runs against live in `seams.rs`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use parking_lot::{Mutex, RwLock};

use crate::index::{EntryId, VolumeIndex};
use crate::metrics::Counters;
use crate::query::{CompiledQuery, QueryOptions};

use super::seams::SnapshotStore;
use super::{Engine, VolumeState};

/// Last materialized per-volume result, kept for incremental refinement
/// (query/subsume.rs) and unchanged-result detection (`QueryTrace::
/// unchanged`). Validity = both generations still match; USN batches
/// invalidate implicitly by bumping `content_generation`.
pub(super) struct VolumeQueryCache {
    /// The raw query text — equality here (with `opt`) defines "the same
    /// query" for unchanged detection; subsumption defines refinement.
    pub(super) text: String,
    pub(super) compiled: Arc<CompiledQuery>,
    pub(super) opt: QueryOptions,
    pub(super) content_generation: u64,
    pub(super) structural_generation: u64,
    pub(super) ids: Arc<Vec<EntryId>>,
}

/// USN position paired with the index state, shared with `Engine::flush`.
/// The tailing thread owns the journal handle; "save now" from another
/// thread needs (`journal_id`, `next_usn`) without touching it. Updated *after*
/// a batch is applied, so a concurrent flush that reads the checkpoint
/// first always saves checkpoint ≤ index — the USN replay on load covers
/// the gap (re-applying records is idempotent; skipping them would not be).
#[derive(Clone, Copy)]
pub(super) struct JournalCheckpoint {
    pub(super) journal_id: u64,
    pub(super) next_usn: i64,
}

pub(super) struct VolumeSlot {
    pub(super) label: String,
    pub(super) phase: Mutex<VolumeState>,
    pub(super) scanned: Mutex<u64>,
    pub(super) index: RwLock<Option<VolumeIndex>>,
    pub(super) stop: Arc<AtomicBool>,
    /// Single-entry query cache (lock order: `index` read first, then this).
    pub(super) last_query: Mutex<Option<VolumeQueryCache>>,
    /// None until the volume is Ready (flush skips it).
    pub(super) checkpoint: Mutex<Option<JournalCheckpoint>>,
    /// (content, structural) generations at the last snapshot save — the
    /// dirty check that keeps periodic flushes from rewriting unchanged
    /// volumes.
    pub(super) last_saved: Mutex<Option<(u64, u64)>>,
    /// Serializes snapshot writers for this slot (flush vs. stop-save).
    pub(super) save_lock: Mutex<()>,
    /// Snapshot persistence seam for this volume (ADR-0018) — production
    /// is `WinSnapshotStore` on `snapshot_path(...)`.
    pub(super) store: Arc<dyn SnapshotStore>,
}

impl VolumeSlot {
    /// A slot in its initial Scanning state, before any index exists —
    /// the shape `index_start` (and the worker tests) spawn workers on.
    pub(super) fn scanning(label: String, store: Arc<dyn SnapshotStore>) -> Self {
        Self {
            label,
            phase: Mutex::new(VolumeState::Scanning),
            scanned: Mutex::new(0),
            index: RwLock::new(None),
            stop: Arc::new(AtomicBool::new(false)),
            last_query: Mutex::new(None),
            checkpoint: Mutex::new(None),
            last_saved: Mutex::new(None),
            save_lock: Mutex::new(()),
            store,
        }
    }

    /// Install a freshly built index. Replacing an existing one is a
    /// structural change (journal-gone full rescan): the new index inherits
    /// the previous `structural_generation + 1` so open `ResultSet`s go
    /// hard-stale (docs/ARCHITECTURE.md, generation 2層). A first install
    /// (initial scan or snapshot restore) keeps the value the index was
    /// built with.
    pub(super) fn install_index(&self, mut idx: VolumeIndex) {
        let mut guard = self.index.write();
        if let Some(prev) = guard.as_ref() {
            idx.bump_structural_from(prev.structural_generation());
        }
        // Generation checks already reject it, but holding onto a dead
        // index's id list (4B × entries) serves nobody.
        *self.last_query.lock() = None;
        *guard = Some(idx);
    }
}

impl Engine {
    /// Fixed NTFS volumes ("C:", "D:", …).
    #[cfg(windows)]
    #[must_use]
    pub fn list_ntfs_volumes() -> Vec<String> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        };
        const DRIVE_FIXED: u32 = 3;
        let mut out = Vec::new();
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
            unsafe {
                if GetDriveTypeW(root.as_ptr()) != DRIVE_FIXED {
                    continue;
                }
                let mut fs = [0u16; 32];
                let ok = GetVolumeInformationW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                );
                if ok != 0 {
                    let fs_name: String = String::from_utf16_lossy(
                        &fs[..fs.iter().position(|&c| c == 0).unwrap_or(0)],
                    );
                    if fs_name == "NTFS" {
                        out.push(format!("{letter}:"));
                    }
                }
            }
        }
        out
    }

    /// Writes the slot's snapshot (via its `SnapshotStore`) under the
    /// per-slot save lock and records the saved generations (the flush
    /// dirty check). Returns false on a failed write — already counted and
    /// logged here.
    #[cfg(windows)]
    pub(super) fn save_slot(&self, slot: &VolumeSlot, checkpoint: JournalCheckpoint) -> bool {
        let _writer = slot.save_lock.lock();
        let guard = slot.index.read();
        let Some(idx) = guard.as_ref() else {
            return false;
        };
        let generations = (idx.content_generation(), idx.structural_generation());
        if let Err(e) = slot
            .store
            .save_atomic(idx, checkpoint.journal_id, checkpoint.next_usn)
        {
            Counters::bump(&self.metrics.counters.snapshot_save_failures);
            tracing::warn!(volume = %slot.label, error = %e, "snapshot save failed");
            return false;
        }
        *slot.last_saved.lock() = Some(generations);
        true
    }
}

/// `{index_dir}\{drive-letter}.fmfidx` — the path each volume's
/// `WinSnapshotStore` is built on.
pub(super) fn snapshot_path(index_dir: &std::path::Path, label: &str) -> std::path::PathBuf {
    index_dir.join(format!(
        "{}.fmfidx",
        label.trim_end_matches(':').to_ascii_lowercase()
    ))
}

/// A drive label is exactly one ASCII letter followed by `':'` ("C:", "d:") —
/// the shape `list_ntfs_volumes` produces and `snapshot_path` expects. This is
/// the trust boundary for [`Engine::index_start`](super::Engine::index_start):
/// validating here bounds the set of distinct labels to a small finite set (so
/// a hostile caller can't spawn unbounded volume threads) and stops a label
/// bearing `..\` or path separators from steering `snapshot_path` outside the
/// index directory.
pub(super) fn is_valid_volume_label(label: &str) -> bool {
    let b = label.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}
