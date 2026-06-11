use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::index::{EntryId, VolumeIndex};
use crate::metrics::{Counters, ScanTrace, UsnTrace};
use crate::query::{CompiledQuery, QueryOptions};

use super::{Engine, EngineEvent, VolumeState};

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
/// thread needs (journal_id, next_usn) without touching it. Updated *after*
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
}

impl VolumeSlot {
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

/// Engine-side debounce for IndexChanged — the only throttle in the whole
/// change path (docs/ARCHITECTURE.md 遅延予算).
const INDEX_CHANGED_DEBOUNCE: Duration = Duration::from_millis(200);

impl Engine {
    /// Fixed NTFS volumes ("C:", "D:", …).
    #[cfg(windows)]
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

    #[cfg(windows)]
    /// Panic firewall: a crashing volume thread must never leave the UI
    /// stuck on "Scanning" with no explanation. The panic itself is logged
    /// (with backtrace) by the diag hook; this converts it into a visible
    /// Failed state.
    pub(super) fn volume_thread(self: Arc<Self>, slot: Arc<VolumeSlot>) {
        let this = self.clone();
        let slot2 = slot.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            this.volume_thread_inner(slot2);
        }));
        if result.is_err() {
            *slot.phase.lock() = VolumeState::Failed;
            self.emit(EngineEvent::VolumeFailed {
                volume: slot.label.clone(),
                message: "internal panic — engine.log に詳細".to_string(),
            });
        }
    }

    #[cfg(windows)]
    fn volume_thread_inner(self: Arc<Self>, slot: Arc<VolumeSlot>) {
        use crate::usn::{ReadOutcome, UsnJournal, VolumeStatFetcher, apply_batch};

        let label = slot.label.clone();
        let snapshot_path = snapshot_path(&self.config.index_dir, &label);

        loop {
            if slot.stop.load(Ordering::Relaxed) {
                return;
            }
            // 1. Journal first (checkpoint precedes the scan so nothing is
            //    missed), then snapshot-or-scan.
            let mut journal = match UsnJournal::open(&label, None) {
                Ok(j) => j,
                Err(e) => {
                    *slot.phase.lock() = VolumeState::Failed;
                    self.emit(EngineEvent::VolumeFailed {
                        volume: label.clone(),
                        message: e.to_string(),
                    });
                    return;
                }
            };

            let load_stage = std::time::Instant::now();
            let loaded = match VolumeIndex::load_from(&snapshot_path) {
                Ok(t) => Some(t),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None, // first run
                Err(e) => {
                    // Corrupt/unreadable snapshot: rescanning recovers, but
                    // the fact must not vanish.
                    Counters::bump(&self.metrics.counters.snapshot_load_failures);
                    tracing::warn!(volume = %label, error = %e, "snapshot unusable — full scan");
                    None
                }
            }
            .filter(|(_, journal_id, next_usn)| match journal.query() {
                Ok(data) => {
                    let valid = *journal_id == data.UsnJournalID && *next_usn >= data.FirstUsn;
                    if !valid {
                        tracing::info!(volume = %label, "snapshot checkpoint stale — full scan");
                    }
                    valid
                }
                Err(_) => false,
            });
            let load_ms = load_stage.elapsed().as_millis() as u64;

            let idx = match loaded {
                Some((idx, _journal_id, next_usn)) => {
                    journal.next_usn = next_usn;
                    tracing::info!(volume = %label, entries = idx.len(), ms = load_ms, "snapshot restored");
                    let file_bytes = std::fs::metadata(&snapshot_path).map_or(0, |m| m.len());
                    self.metrics.record_scan(ScanTrace {
                        volume: label.clone(),
                        source: "snapshot".to_string(),
                        read_bytes: file_bytes,
                        read_ms: 0,
                        mb_per_s: 0.0,
                        parse_ms: 0,
                        deferred_ms: 0,
                        build_ms: 0,
                        sort_ms: 0,
                        total_ms: load_ms,
                        entries: idx.len() as u64,
                        peak_ws_bytes: crate::mft::peak_working_set(),
                    });
                    idx
                }
                None => match crate::mft::scan_volume(&label) {
                    Ok((mut idx, stats)) => {
                        tracing::info!(
                            volume = %label,
                            entries = idx.len(),
                            ms = stats.elapsed_total_ms,
                            "full scan complete"
                        );
                        idx.shrink_to_fit();
                        Counters::add(
                            &self.metrics.counters.corrupt_mft_records,
                            stats.corrupt_records,
                        );
                        Counters::add(
                            &self.metrics.counters.deferred_names_unresolved,
                            stats.deferred_unresolved,
                        );
                        Counters::add(
                            &self.metrics.counters.scan_pipeline_fallbacks,
                            stats.pipeline_fallbacks,
                        );
                        self.metrics.record_scan(ScanTrace {
                            volume: label.clone(),
                            source: "scan".to_string(),
                            read_bytes: stats.mft_bytes,
                            read_ms: stats.elapsed_mft_load_ms,
                            mb_per_s: if stats.elapsed_mft_load_ms > 0 {
                                stats.mft_bytes as f64
                                    / 1_048.576
                                    / stats.elapsed_mft_load_ms as f64
                            } else {
                                0.0
                            },
                            parse_ms: stats.elapsed_parse_ms,
                            deferred_ms: stats.elapsed_deferred_ms,
                            build_ms: stats.elapsed_build_ms,
                            sort_ms: stats.elapsed_sort_ms,
                            total_ms: stats.elapsed_total_ms,
                            entries: idx.len() as u64,
                            peak_ws_bytes: stats.peak_working_set_bytes,
                        });
                        idx
                    }
                    Err(e) => {
                        *slot.phase.lock() = VolumeState::Failed;
                        self.emit(EngineEvent::VolumeFailed {
                            volume: label.clone(),
                            message: e.to_string(),
                        });
                        return;
                    }
                },
            };

            let entries = idx.live_len() as u64;
            *slot.scanned.lock() = entries;
            slot.install_index(idx);
            // Scan path: next_usn is the position at journal open (before the
            // scan), so a flush taken now replays the scan window — correct,
            // just slightly redundant.
            *slot.checkpoint.lock() = Some(JournalCheckpoint {
                journal_id: journal.journal_id,
                next_usn: journal.next_usn,
            });
            *slot.phase.lock() = VolumeState::Ready;
            self.emit(EngineEvent::VolumeReady {
                volume: label.clone(),
                entries,
            });
            // Prewarm the query accelerators (dir-path memo, offset table)
            // so the first keystroke never pays the cold-cache cost.
            if let Some(idx) = slot.index.read().as_ref() {
                crate::query::prewarm(idx);
            }

            // 2. Tail the journal until stop or journal-gone.
            let fetch = match VolumeStatFetcher::open(&label) {
                Ok(f) => f,
                Err(e) => {
                    self.emit(EngineEvent::VolumeFailed {
                        volume: label.clone(),
                        message: e.to_string(),
                    });
                    return;
                }
            };
            let mut buf = Vec::new();
            let mut last_emit = Instant::now() - INDEX_CHANGED_DEBOUNCE;
            loop {
                if slot.stop.load(Ordering::Relaxed) {
                    self.save_slot(
                        &slot,
                        JournalCheckpoint {
                            journal_id: journal.journal_id,
                            next_usn: journal.next_usn,
                        },
                        &snapshot_path,
                    );
                    return;
                }
                match journal.read_blocking(&mut buf) {
                    Ok(ReadOutcome::Records {
                        records: rs,
                        truncated,
                    }) => {
                        if truncated {
                            Counters::bump(&self.metrics.counters.usn_batches_truncated);
                            tracing::warn!(volume = %label, "USN batch had malformed tail bytes");
                        }
                        if rs.is_empty() {
                            continue;
                        }
                        if let Some(idx) = slot.index.write().as_mut() {
                            let stage = crate::metrics::Stage::start();
                            let s = apply_batch(idx, &rs, &fetch);
                            Counters::add(
                                &self.metrics.counters.stat_fetch_failures,
                                s.stat_failures as u64,
                            );
                            self.metrics.record_usn(UsnTrace {
                                volume: label.clone(),
                                records: rs.len() as u64,
                                upserted: s.created_or_renamed as u64,
                                deleted: s.deleted as u64,
                                stat_updated: s.stat_updated as u64,
                                stat_failures: s.stat_failures as u64,
                                apply_us: stage.elapsed_us(),
                            });
                            *slot.scanned.lock() = idx.live_len() as u64;
                        }
                        // Index first, checkpoint second (see JournalCheckpoint).
                        *slot.checkpoint.lock() = Some(JournalCheckpoint {
                            journal_id: journal.journal_id,
                            next_usn: journal.next_usn,
                        });
                        self.maybe_compact(&slot);
                        if last_emit.elapsed() >= INDEX_CHANGED_DEBOUNCE {
                            last_emit = Instant::now();
                            self.emit(EngineEvent::IndexChanged {
                                volume: label.clone(),
                            });
                        }
                    }
                    Ok(ReadOutcome::Gone(gone)) => {
                        Counters::bump(&self.metrics.counters.journal_rescans);
                        tracing::warn!(volume = %label, ?gone, "journal gone — full rescan");
                        // The old journal id is dead; a flush during the
                        // rescan must not pair it with the new index.
                        *slot.checkpoint.lock() = None;
                        *slot.phase.lock() = VolumeState::Rescanning;
                        self.emit(EngineEvent::RescanStarted {
                            volume: label.clone(),
                        });
                        let _ = std::fs::remove_file(&snapshot_path);
                        break; // restart the outer loop → fresh journal + scan
                    }
                    Err(e) => {
                        *slot.phase.lock() = VolumeState::Failed;
                        self.emit(EngineEvent::VolumeFailed {
                            volume: label.clone(),
                            message: e.to_string(),
                        });
                        return;
                    }
                }
            }
        }
    }

    /// Compact once the tombstone/garbage thresholds trip (checked per
    /// applied USN batch). The copy builds under a *read* guard — this
    /// volume thread is the index's only writer — and the write lock is
    /// held for the swap alone. `install_index` bumps the structural
    /// generation, hard-staling open result handles.
    fn maybe_compact(&self, slot: &VolumeSlot) {
        let compacted = {
            let guard = slot.index.read();
            let Some(idx) = guard.as_ref().filter(|idx| idx.compaction_due()) else {
                return;
            };
            let stage = crate::metrics::Stage::start();
            let generation = idx.content_generation();
            let dropped = idx.len() - idx.live_len();
            let new_idx = idx.compacted();
            tracing::info!(
                volume = %slot.label,
                dropped_entries = dropped,
                reclaimed_name_bytes = idx.stats(&slot.label).dead_name_bytes,
                ms = stage.elapsed_us() / 1000,
                "index compacted"
            );
            (new_idx, generation)
        };
        // Single-writer invariant: nothing can have advanced the generation
        // between copy and swap. If it ever does, installing would lose
        // those mutations — drop the copy loudly instead.
        let guard = slot.index.read();
        let current = guard.as_ref().map(|i| i.content_generation());
        drop(guard);
        if current != Some(compacted.1) {
            Counters::bump(&self.metrics.counters.compaction_aborts);
            tracing::warn!(
                volume = %slot.label,
                "index mutated during compaction — copy discarded"
            );
            return;
        }
        slot.install_index(compacted.0);
        if let Some(idx) = slot.index.read().as_ref() {
            crate::query::prewarm(idx);
        }
    }

    /// Writes the slot's snapshot under the per-slot save lock and records
    /// the saved generations (the flush dirty check). Returns false on a
    /// failed write — already counted and logged here.
    #[cfg(windows)]
    pub(super) fn save_slot(
        &self,
        slot: &VolumeSlot,
        checkpoint: JournalCheckpoint,
        path: &std::path::Path,
    ) -> bool {
        let _writer = slot.save_lock.lock();
        let guard = slot.index.read();
        let Some(idx) = guard.as_ref() else {
            return false;
        };
        let generations = (idx.content_generation(), idx.structural_generation());
        if let Err(e) = idx.save_to(path, checkpoint.journal_id, checkpoint.next_usn) {
            Counters::bump(&self.metrics.counters.snapshot_save_failures);
            tracing::warn!(volume = %slot.label, error = %e, "snapshot save failed");
            return false;
        }
        *slot.last_saved.lock() = Some(generations);
        true
    }
}

/// `{index_dir}\{drive-letter}.fmfidx` — shared by the tailing thread's
/// stop-save and `Engine::flush`.
pub(super) fn snapshot_path(index_dir: &std::path::Path, label: &str) -> std::path::PathBuf {
    index_dir.join(format!(
        "{}.fmfidx",
        label.trim_end_matches(':').to_ascii_lowercase()
    ))
}
