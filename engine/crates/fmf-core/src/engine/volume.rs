use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::index::VolumeIndex;
use crate::metrics::{Counters, ScanTrace, UsnTrace};

use super::{Engine, EngineEvent, VolumePhase};

pub(super) struct VolumeSlot {
    pub(super) label: String,
    pub(super) phase: Mutex<VolumePhase>,
    pub(super) scanned: Mutex<u64>,
    pub(super) index: RwLock<Option<VolumeIndex>>,
    pub(super) stop: Arc<AtomicBool>,
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
            *slot.phase.lock() = VolumePhase::Failed;
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
        let snapshot_path = self.config.index_dir.join(format!(
            "{}.fmfidx",
            label.trim_end_matches(':').to_ascii_lowercase()
        ));

        loop {
            if slot.stop.load(Ordering::Relaxed) {
                return;
            }
            // 1. Journal first (checkpoint precedes the scan so nothing is
            //    missed), then snapshot-or-scan.
            let mut journal = match UsnJournal::open(&label, None) {
                Ok(j) => j,
                Err(e) => {
                    *slot.phase.lock() = VolumePhase::Failed;
                    self.emit(EngineEvent::VolumeFailed {
                        volume: label.clone(),
                        message: e.to_string(),
                    });
                    return;
                }
            };

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

            let idx = match loaded {
                Some((idx, _journal_id, next_usn)) => {
                    journal.next_usn = next_usn;
                    tracing::info!(volume = %label, entries = idx.len(), "snapshot restored");
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
                        self.metrics.record_scan(ScanTrace {
                            volume: label.clone(),
                            read_bytes: stats.mft_bytes,
                            read_ms: stats.elapsed_mft_load_ms,
                            mb_per_s: if stats.elapsed_mft_load_ms > 0 {
                                stats.mft_bytes as f64
                                    / 1_048.576
                                    / stats.elapsed_mft_load_ms as f64
                            } else {
                                0.0
                            },
                            parse_ms: stats
                                .elapsed_total_ms
                                .saturating_sub(stats.elapsed_mft_load_ms),
                            build_ms: 0,
                            sort_ms: 0,
                            total_ms: stats.elapsed_total_ms,
                            entries: idx.len() as u64,
                            peak_ws_bytes: stats.peak_working_set_bytes,
                        });
                        idx
                    }
                    Err(e) => {
                        *slot.phase.lock() = VolumePhase::Failed;
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
            *slot.index.write() = Some(idx);
            *slot.phase.lock() = VolumePhase::Ready;
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
                    self.save_slot(&slot, &journal, &snapshot_path);
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
                        *slot.phase.lock() = VolumePhase::Rescanning;
                        self.emit(EngineEvent::RescanStarted {
                            volume: label.clone(),
                        });
                        let _ = std::fs::remove_file(&snapshot_path);
                        break; // restart the outer loop → fresh journal + scan
                    }
                    Err(e) => {
                        *slot.phase.lock() = VolumePhase::Failed;
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

    #[cfg(windows)]
    fn save_slot(
        &self,
        slot: &VolumeSlot,
        journal: &crate::usn::UsnJournal,
        path: &std::path::Path,
    ) {
        if let Some(idx) = slot.index.read().as_ref()
            && let Err(e) = idx.save_to(path, journal.journal_id, journal.next_usn)
        {
            Counters::bump(&self.metrics.counters.snapshot_save_failures);
            tracing::warn!(volume = %slot.label, error = %e, "snapshot save failed");
        }
    }
}
