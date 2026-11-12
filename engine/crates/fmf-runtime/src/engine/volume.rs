//! Per-volume state: the slot the engine and the worker thread share (`VolumeSlot`), the index install rule, the journal checkpoint, and the snapshot save helper.
//! The thread that drives the flow lives in `worker.rs`; the OS-effect seams it runs against live in `seams.rs`.

#[cfg(windows)]
const DRIVE_LETTER_COUNT: u32 = 26;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Instant;

use fmf_core::recovery::{FailureKind, RetryState, backoff_remaining};
use parking_lot::{Condvar, Mutex, RwLock};

use crate::index::{EntryId, VolumeIndex};
use crate::metrics::Counters;
use crate::query::{CompiledQuery, QueryOptions};
use crate::volume_label::VolumeLabel;

use super::seams::SnapshotStore;
use super::{Engine, VolumeState};

/// Last materialized per-volume result, kept only for incremental refinement (`query/subsume.rs`).
/// Presentation identity is an explicit live result handle, never this engine-global cache (ADR-0044).
/// Validity = both generations still match; USN batches invalidate implicitly by bumping `content_generation`.
pub(super) struct VolumeQueryCache {
    pub(super) compiled: Arc<CompiledQuery>,
    pub(super) opt: QueryOptions,
    pub(super) content_generation: u64,
    pub(super) structural_generation: u64,
    pub(super) ids: Arc<[EntryId]>,
}

/// USN position paired with its index under the slot's read/write lock.
#[derive(Clone, Copy)]
pub(super) struct JournalCheckpoint {
    pub(super) journal_id: u64,
    pub(super) next_usn: i64,
}

#[derive(Default)]
pub(super) struct VolumeData {
    index: Option<VolumeIndex>,
    checkpoint: Option<JournalCheckpoint>,
}

impl VolumeData {
    pub(super) const fn as_ref(&self) -> Option<&VolumeIndex> {
        self.index.as_ref()
    }

    pub(super) const fn as_mut(&mut self) -> Option<&mut VolumeIndex> {
        self.index.as_mut()
    }

    pub(super) const fn set_checkpoint(&mut self, checkpoint: Option<JournalCheckpoint>) {
        self.checkpoint = checkpoint;
    }

    fn replace_index(&mut self, mut index: VolumeIndex) {
        if let Some(previous) = &self.index {
            index.bump_structural_from(previous.structural_generation());
        }
        self.index = Some(index);
    }
}

pub(super) enum SnapshotSave {
    IfDirty,
    Always,
}

#[derive(Clone, Copy)]
pub(super) enum RetryControl {
    AutomaticAvailable,
    AutomaticSpent,
    Backoff(Instant),
    AwaitRequest,
}

impl RetryControl {
    pub(super) const fn policy(self) -> RetryState {
        match self {
            Self::AutomaticAvailable => RetryState::AutomaticAvailable,
            Self::AutomaticSpent => RetryState::AutomaticSpent,
            Self::Backoff(_) => RetryState::Backoff,
            Self::AwaitRequest => RetryState::AwaitRequest,
        }
    }

    fn transition(&mut self, state: RetryState) {
        *self = match state {
            RetryState::AutomaticAvailable => Self::AutomaticAvailable,
            RetryState::AutomaticSpent => Self::AutomaticSpent,
            RetryState::Backoff => Self::Backoff(Instant::now()),
            RetryState::AwaitRequest => Self::AwaitRequest,
        };
    }
}

pub(super) struct VolumeLifecycle {
    pub(super) state: VolumeState,
    pub(super) retry: RetryControl,
}

impl VolumeLifecycle {
    pub(super) fn failed(&mut self, kind: FailureKind) {
        self.retry.transition(self.retry.policy().failed(kind));
        self.state = VolumeState::Failed;
    }
}

#[cfg(test)]
pub(super) struct SnapshotReadGate {
    pub(super) entered: std::sync::mpsc::SyncSender<()>,
    pub(super) resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
impl SnapshotReadGate {
    pub(super) fn wait(self) {
        self.entered
            .send(())
            .expect("announce pending snapshot read");
        self.resume
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("release pending snapshot read");
    }
}

pub(super) struct VolumeSlot {
    pub(super) label: String,
    pub(super) phase: Mutex<VolumeLifecycle>,
    pub(super) retry_wake: Condvar,
    pub(super) scanned: Mutex<u64>,
    pub(super) index: RwLock<VolumeData>,
    pub(super) stop: Arc<AtomicBool>,
    /// Single-entry query cache (lock order: `index` read first, then this).
    pub(super) last_query: Mutex<Option<VolumeQueryCache>>,
    /// (content, structural) generations at the last snapshot save — the dirty check that keeps periodic flushes from rewriting unchanged volumes.
    pub(super) last_saved: Mutex<Option<(u64, u64)>>,
    /// Serializes snapshot writers for this slot (flush vs. stop-save).
    pub(super) save_lock: Mutex<()>,
    #[cfg(test)]
    pub(super) snapshot_read_gate: Mutex<Option<SnapshotReadGate>>,
    /// Snapshot persistence seam for this volume (ADR-0018) — production is `WinSnapshotStore` on `snapshot_path(...)`.
    pub(super) store: Arc<dyn SnapshotStore>,
}

impl VolumeSlot {
    /// A privileged ($MFT + USN) slot in its initial Scanning state — the shape `index_start` (and the worker tests) spawn workers on.
    pub(super) fn scanning(label: String, store: Arc<dyn SnapshotStore>) -> Self {
        Self {
            label,
            phase: Mutex::new(VolumeLifecycle {
                state: VolumeState::Scanning,
                retry: RetryControl::AutomaticAvailable,
            }),
            retry_wake: Condvar::new(),
            scanned: Mutex::new(0),
            index: RwLock::new(VolumeData::default()),
            stop: Arc::new(AtomicBool::new(false)),
            last_query: Mutex::new(None),
            last_saved: Mutex::new(None),
            save_lock: Mutex::new(()),
            #[cfg(test)]
            snapshot_read_gate: Mutex::new(None),
            store,
        }
    }

    /// Install a freshly built index.
    /// Replacing an existing one is a structural change (journal-gone full rescan): the new index inherits the previous `structural_generation + 1` so open `ResultSet`s go hard stale.
    /// A first install (initial scan or snapshot restore) keeps the value the index was built with — there are no handles to invalidate.
    ///
    /// Every index replacement goes through here, which is what makes the generation bump impossible to forget.
    pub(super) fn install_index(&self, idx: VolumeIndex) {
        let mut guard = self.index.write();
        guard.replace_index(idx);
        *self.last_query.lock() = None;
    }

    pub(super) fn install_snapshot(&self, idx: VolumeIndex, checkpoint: JournalCheckpoint) {
        let mut guard = self.index.write();
        guard.replace_index(idx);
        guard.checkpoint = Some(checkpoint);
        *self.last_query.lock() = None;
    }

    pub(super) fn request_retry(&self) {
        let mut lifecycle = self.phase.lock();
        if lifecycle.state != VolumeState::Failed || self.stop.load(Ordering::Relaxed) {
            return;
        }
        let next = lifecycle.retry.policy().requested();
        lifecycle.retry.transition(next);
        lifecycle.state = VolumeState::Scanning;
        self.retry_wake.notify_one();
    }

    pub(super) fn wait_for_retry(&self) -> bool {
        let mut lifecycle = self.phase.lock();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            match lifecycle.retry {
                RetryControl::AutomaticAvailable | RetryControl::AutomaticSpent => {
                    lifecycle.state = VolumeState::Scanning;
                    return true;
                }
                RetryControl::Backoff(started) => {
                    let remaining = backoff_remaining(started.elapsed());
                    if remaining.is_zero() {
                        let next = lifecycle.retry.policy().elapsed();
                        lifecycle.retry.transition(next);
                    } else {
                        self.retry_wake.wait_for(&mut lifecycle, remaining);
                    }
                }
                RetryControl::AwaitRequest => self.retry_wake.wait(&mut lifecycle),
            }
        }
    }

    pub(super) fn request_stop(&self) {
        let _lifecycle = self.phase.lock();
        self.stop.store(true, Ordering::Relaxed);
        self.retry_wake.notify_all();
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
        for i in 0..DRIVE_LETTER_COUNT {
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

    /// Writes the slot's snapshot (via its `SnapshotStore`) under the per-slot save lock and records the saved generations (the flush dirty check).
    /// Returns false on a failed write — already counted and logged here.
    #[cfg(windows)]
    pub(super) fn save_slot(&self, slot: &VolumeSlot, mode: SnapshotSave) -> bool {
        let _writer = slot.save_lock.lock();
        let guard = slot.index.read();
        let (Some(idx), Some(checkpoint)) = (guard.as_ref(), guard.checkpoint) else {
            return false;
        };
        let generations = (idx.content_generation(), idx.structural_generation());
        if matches!(mode, SnapshotSave::IfDirty) && *slot.last_saved.lock() == Some(generations) {
            return false;
        }
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

/// `{index_dir}\{drive-letter}.fmfidx` — the path each volume's `WinSnapshotStore` is built on.
pub(super) fn snapshot_path(index_dir: &std::path::Path, label: &str) -> std::path::PathBuf {
    index_dir.join(format!(
        "{}.fmfidx",
        label.trim_end_matches(':').to_ascii_lowercase()
    ))
}

/// A drive label is exactly one ASCII letter followed by `':'` ("C:", "d:") —
/// the shape `list_ntfs_volumes` produces and `snapshot_path` expects.
/// This is the trust boundary for [`Engine::index_start`](super::Engine::index_start):
/// validating here bounds the set of distinct labels to a small finite set (so a hostile caller can't spawn unbounded volume threads) and stops a label bearing `..\` or path separators from steering `snapshot_path` outside the index directory.
pub(super) fn is_valid_volume_label(label: &str) -> bool {
    VolumeLabel::parse(label).is_some()
}

/// Validate one whole `IndexStart` request before the engine creates any slots.
/// Returned labels are uppercase and unique in request order.
pub(super) fn validate_index_start_volumes(
    requested: &[String],
    available: &[String],
) -> Result<Vec<String>, super::IndexStartError> {
    let available: Vec<String> = available
        .iter()
        .filter_map(|label| VolumeLabel::parse(label))
        .map(|label| label.to_string())
        .collect();
    let mut canonical = Vec::with_capacity(requested.len());
    for (position, label) in requested.iter().enumerate() {
        let Some(label) = VolumeLabel::parse(label) else {
            return Err(super::IndexStartError::MalformedLabel { position });
        };
        let label = label.to_string();
        if canonical.contains(&label) {
            return Err(super::IndexStartError::DuplicateLabel { label });
        }
        if !available.contains(&label) {
            return Err(super::IndexStartError::UnavailableVolume { label });
        }
        canonical.push(label);
    }
    Ok(canonical)
}
