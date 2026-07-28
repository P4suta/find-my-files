//! The volume worker: the thread that drives one volume through
//! restore-or-scan → Ready → USN tailing → (journal-gone) rescan, forever.
//! `volume.rs` is the state's home (`VolumeSlot`, checkpoint, save helper);
//! this file is the flow's home. Decisions are pure functions; effects
//! (counters, logs, events, installs, saves) stay in the loop, keyed off
//! the decisions — that split is what lets `worker_tests.rs` replay the
//! failure paths deterministically without elevation (ADR-0018, S4b).
//!
//! The full $MFT scan itself is deliberately *not* behind a seam (the
//! 2-trait cap): its execution stays real-volume territory, covered by the
//! `FMF_ADMIN_TESTS` layer.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::metrics::{Counters, ScanTrace, UsnTrace};
use crate::usn::apply::{apply_planned, plan_batch};
use crate::usn::{JournalGone, ReadOutcome, UsnError, UsnRecord};

use super::seams::{JournalSource, JournalView, WinJournalSource};
use super::volume::{JournalCheckpoint, VolumeSlot};
use super::{Engine, EngineEvent, VolumeState};

/// Engine-side debounce for `IndexChanged` — the only event-rate throttle in
/// the whole change path, deliberately placed here so no consumer adds a
/// second one.
///
/// The tail loop's ≤250ms idle-edge read park is a discovery-latency floor,
/// not a throttle. Together with a ≤100ms batch commit, a ≤100ms UI re-query
/// and ≤100ms of render, this 200ms is what keeps the worst-case
/// change-to-screen latency inside the ≤1s acceptance criterion (≤500ms once
/// the volume is busy, since a busy tail loop never parks).
const INDEX_CHANGED_DEBOUNCE: Duration = Duration::from_millis(200);

/// How the worker establishes a volume's index at the top of its loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapshotDecision {
    /// Install the loaded snapshot and replay the journal from its
    /// persisted cursor.
    Restore,
    /// Build the index from a full $MFT scan.
    FullScan(FullScanReason),
}

/// Why a full scan was chosen — selects the effect at the call site
/// (counter + warn / info / silence). Effects stay out of the decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FullScanReason {
    /// No snapshot on disk: the normal first run, not a failure.
    FirstRun,
    /// Snapshot present but unreadable/corrupt — counted in
    /// `snapshot_load_failures`, never silent.
    SnapshotUnusable,
    /// Snapshot loaded, but its checkpoint cannot be replayed from the
    /// live journal: the journal id changed, or the persisted cursor was
    /// already purged (`next_usn < first_usn`).
    CheckpointStale,
    /// `FSCTL_QUERY_USN_JOURNAL` failed, so the checkpoint cannot be
    /// validated and must not be trusted. Silent by design (pre-seam
    /// behavior): a journal this broken fails the next open/read loudly.
    JournalQueryFailed,
}

/// Pure decision: snapshot-load outcome × live-journal view → restore or
/// full scan. The load result carries only the persisted checkpoint; the
/// index itself never enters the decision.
///
/// `journal` is `None` when `FSCTL_QUERY` failed *or* was skipped because
/// the load already failed (the worker never queries in that case — a
/// failed load must not spend an FSCTL). The load arm is matched first,
/// so the two `None` meanings cannot mix.
pub(super) fn snapshot_decision(
    load: Result<(u64, i64), &std::io::Error>,
    journal: Option<JournalView>,
) -> SnapshotDecision {
    match load {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            SnapshotDecision::FullScan(FullScanReason::FirstRun)
        }
        Err(_) => SnapshotDecision::FullScan(FullScanReason::SnapshotUnusable),
        Ok((journal_id, next_usn)) => match journal {
            Some(view) if journal_id == view.journal_id && next_usn >= view.first_usn => {
                SnapshotDecision::Restore
            }
            Some(_) => SnapshotDecision::FullScan(FullScanReason::CheckpointStale),
            None => SnapshotDecision::FullScan(FullScanReason::JournalQueryFailed),
        },
    }
}

/// What one blocking-read outcome means for the tail loop.
pub(super) enum TailStep {
    /// Apply the records to the index, then publish the new checkpoint —
    /// in that order (see the checkpoint-after-apply invariant at the
    /// apply site).
    Apply(Vec<UsnRecord>),
    /// The journal id is dead. Recovery is always a full rescan
    /// (docs/RESEARCH.md standard practice): invalidate the shared checkpoint, drop the
    /// snapshot, announce Rescanning, restart the outer loop.
    Rescan(RescanCause),
    /// Unrecoverable read error — the volume goes Failed.
    Fail(UsnError),
}

/// Why tailing must be abandoned and rebuilt from a fresh journal position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RescanCause {
    /// The OS reported that the journal/checkpoint can no longer be replayed.
    JournalGone(JournalGone),
    /// The FSCTL payload was malformed. Applying its valid prefix and advancing
    /// the cursor would permanently lose the dropped suffix.
    MalformedBatch,
    /// Complete live hard-link metadata was unavailable, so the journal batch
    /// was rejected atomically rather than checkpointed with stale paths.
    MetadataRefreshFailed,
    /// The Ready-state index unexpectedly disappeared before a journal batch
    /// could be applied. Advancing its checkpoint would lose the batch.
    IndexUnavailable,
    /// The live index refused a journal record because applying it would have
    /// broken a topology invariant. Checkpointing past it would leave the
    /// index permanently disagreeing with the volume.
    IndexRejected,
}

/// Pure decision: classify one blocking-read outcome into the worker's
/// next step. Every `JournalGone` variant maps to a rescan — none is
/// recoverable in place — and an FSCTL error is fatal for the volume.
pub(super) fn journal_gone_action(outcome: Result<ReadOutcome, UsnError>) -> TailStep {
    match outcome {
        Ok(ReadOutcome::Records {
            truncated: true, ..
        }) => TailStep::Rescan(RescanCause::MalformedBatch),
        Ok(ReadOutcome::Records { records, .. }) => TailStep::Apply(records),
        Ok(ReadOutcome::Gone(gone)) => TailStep::Rescan(RescanCause::JournalGone(gone)),
        Err(e) => TailStep::Fail(e),
    }
}

/// Outcome of the compaction generation recheck (pure half of
/// `maybe_compact`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompactionVerdict {
    Install,
    Abort,
}

/// Pure decision: the compacted copy was built under a read guard at
/// content generation `copied_at`; installing it is only sound if nothing
/// advanced the generation in between. Single-writer invariant: this
/// volume thread is the index's only writer, so `Abort` means that
/// invariant broke somewhere and installing would silently lose the
/// in-between mutations — the copy is discarded loudly instead.
pub(super) fn compact_recheck(copied_at: u64, current: Option<u64>) -> CompactionVerdict {
    if current == Some(copied_at) {
        CompactionVerdict::Install
    } else {
        CompactionVerdict::Abort
    }
}

impl Engine {
    /// Production wiring: the Windows journal seam (the snapshot seam
    /// lives on the slot, created by `index_start`).
    #[cfg(windows)]
    pub(super) fn volume_thread(self: Arc<Self>, slot: Arc<VolumeSlot>) {
        let mut journal = WinJournalSource::new(slot.label.clone());
        self.volume_thread_with(slot, &mut journal);
    }

    /// Panic firewall: a crashing volume thread must never leave the UI
    /// stuck on "Scanning" with no explanation. The panic itself is logged
    /// (with backtrace) by the diag hook; this converts it into a visible
    /// Failed state. Worker entry with an injectable journal seam —
    /// production passes the Win implementation; `worker_tests.rs` passes
    /// scripted fakes.
    #[cfg(windows)]
    pub(super) fn volume_thread_with(
        self: Arc<Self>,
        slot: Arc<VolumeSlot>,
        journal: &mut dyn JournalSource,
    ) {
        let this = self.clone();
        let slot2 = slot.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            this.volume_thread_inner(slot2, journal);
        }));
        if result.is_err() {
            *slot.phase.lock() = VolumeState::Failed;
            self.emit(EngineEvent::VolumeFailed {
                volume: slot.label.clone(),
                message: "internal panic — エンジンログに詳細".to_string(),
            });
        }
    }

    #[cfg(windows)]
    fn volume_thread_inner(
        self: Arc<Self>,
        slot: Arc<VolumeSlot>,
        journal: &mut dyn JournalSource,
    ) {
        let label = slot.label.clone();
        let store = slot.store.clone();

        loop {
            if slot.stop.load(Ordering::Relaxed) {
                return;
            }
            // 1. Journal first (checkpoint precedes the scan so nothing is
            //    missed), then snapshot-or-scan.
            if let Err(e) = journal.open() {
                *slot.phase.lock() = VolumeState::Failed;
                self.emit(EngineEvent::VolumeFailed {
                    volume: label,
                    message: e.to_string(),
                });
                return;
            }

            let load_stage = std::time::Instant::now();
            let load = store.load();
            let journal_view = if load.is_ok() {
                journal.query().ok()
            } else {
                None
            };
            let decision = snapshot_decision(
                load.as_ref()
                    .map(|(_, journal_id, next_usn)| (*journal_id, *next_usn)),
                journal_view,
            );

            let idx = match decision {
                SnapshotDecision::Restore => {
                    let (idx, _journal_id, next_usn) =
                        load.expect("Restore implies a loaded snapshot");
                    journal.set_next_usn(next_usn);
                    let load_ms = load_stage.elapsed().as_millis() as u64;
                    tracing::info!(area = "snapshot", volume = %label, entries = idx.len(), ms = load_ms, "snapshot restored");
                    let file_bytes = store.file_bytes();
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
                SnapshotDecision::FullScan(reason) => {
                    match reason {
                        FullScanReason::SnapshotUnusable => {
                            // Corrupt/unreadable snapshot: rescanning
                            // recovers, but the fact must not vanish.
                            if let Err(e) = &load {
                                crate::degrade!(
                                    self.metrics.counters.snapshot_load_failures,
                                    volume = %label,
                                    error = %e,
                                    "snapshot unusable — full scan"
                                );
                            }
                        }
                        FullScanReason::CheckpointStale => {
                            tracing::info!(volume = %label, "snapshot checkpoint stale — full scan");
                        }
                        FullScanReason::FirstRun | FullScanReason::JournalQueryFailed => {}
                    }
                    // A rejected snapshot must not stay resident while the
                    // scan (and the tail session after it) runs.
                    drop(load);
                    let scanned = crate::mft::scan_volume_cancellable(&label, &slot.stop);
                    match scanned {
                        Ok((mut idx, stats)) => {
                            if slot.stop.load(Ordering::Relaxed) {
                                return;
                            }
                            tracing::info!(
                                volume = %label,
                                entries = idx.len(),
                                ms = stats.elapsed_total_ms,
                                "full scan complete"
                            );
                            idx.shrink_to_fit();
                            // Single stats→counters mapping point: the scan
                            // internals only return degradations in ScanStats,
                            // never warn. A count add is needed, so instead of
                            // degrade! (bump=+1 only) the add and warn sit on two
                            // adjacent explicit lines, done indivisibly.
                            if stats.pipeline_fallbacks > 0 {
                                Counters::add(
                                    &self.metrics.counters.scan_pipeline_fallbacks,
                                    stats.pipeline_fallbacks,
                                );
                                tracing::warn!(
                                    volume = %label,
                                    count = stats.pipeline_fallbacks,
                                    "scan I/O thread unavailable — inline sequential reads"
                                );
                            }
                            if stats.deferred_record_cache_spills > 0 {
                                Counters::add(
                                    &self.metrics.counters.deferred_name_cache_overflow,
                                    stats.deferred_record_cache_spills,
                                );
                                tracing::warn!(
                                    volume = %label,
                                    spilled = stats.deferred_record_cache_spills,
                                    "deferred-record cache full — spilled base/extension records resolved via disk reads"
                                );
                            }
                            if stats.deferred_name_read_failures > 0 {
                                Counters::add(
                                    &self.metrics.counters.deferred_name_read_failures,
                                    stats.deferred_name_read_failures,
                                );
                                tracing::warn!(
                                    volume = %label,
                                    failures = stats.deferred_name_read_failures,
                                    "deferred-name MFT reads failed — authoritative live metadata fallback was used"
                                );
                            }
                            if stats.unresolved_parents > 0 {
                                Counters::add(
                                    &self.metrics.counters.scan_unresolved_parents,
                                    stats.unresolved_parents,
                                );
                                tracing::warn!(
                                    volume = %label,
                                    dropped = stats.unresolved_parents,
                                    "rows dropped: the scan never saw their parent — the journal replay restores whichever still exist"
                                );
                            }
                            if stats.deferred_stat_failures > 0 {
                                Counters::add(
                                    &self.metrics.counters.stat_fetch_failures,
                                    stats.deferred_stat_failures,
                                );
                                tracing::warn!(
                                    volume = %label,
                                    failures = stats.deferred_stat_failures,
                                    "deferred-pass size lookups failed — rows published with the size their base record proves"
                                );
                            }
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
                            tracing::info!(
                                area = "scan",
                                volume = %label,
                                source = "scan",
                                entries = idx.len() as u64,
                                ms = stats.elapsed_total_ms,
                                "volume indexed"
                            );
                            idx
                        }
                        Err(crate::mft::MftError::Cancelled) => {
                            // Only reachable with `stop` set, but say so: a
                            // volume that returns here stays `Scanning`, and a
                            // silent exit from that state is indistinguishable
                            // from a hang.
                            tracing::debug!(
                                area = "scan",
                                volume = %label,
                                "scan cancelled — volume left unscanned"
                            );
                            return;
                        }
                        Err(e) => {
                            if let crate::mft::MftError::CorruptRecords(count) = &e {
                                Counters::add(&self.metrics.counters.corrupt_mft_records, *count);
                                tracing::error!(
                                    volume = %label,
                                    count,
                                    "corrupt MFT records — partial index rejected"
                                );
                            }
                            // Every other failure used to reach a client only as
                            // a `VolumeFailed` event, which is dropped outright
                            // when nothing is connected — precisely the case
                            // during service startup, where the scan runs before
                            // any client arrives. The volume then sat `Failed`
                            // with the log showing nothing at all, which is how
                            // an intermittent scan failure came to look like a
                            // deadlock: the worker had returned, the process was
                            // idle, and no evidence survived anywhere.
                            tracing::error!(
                                area = "scan",
                                volume = %label,
                                reason = e.reason(),
                                error = %crate::diag::error_chain(&e),
                                "volume scan failed — index rejected"
                            );
                            *slot.phase.lock() = VolumeState::Failed;
                            self.emit(EngineEvent::VolumeFailed {
                                volume: label,
                                message: e.to_string(),
                            });
                            return;
                        }
                    }
                }
            };

            if slot.stop.load(Ordering::Relaxed) {
                return;
            }
            // A Ready volume promises that both the immutable base index and
            // every dependency needed to keep it current are wired. Open the
            // live metadata source before publishing/installing the index so
            // a failed handle never creates a transient Ready state.
            let fetch = match journal.open_metadata_source(Arc::clone(&slot.stop)) {
                Ok(fetch) => fetch,
                Err(e) => {
                    *slot.phase.lock() = VolumeState::Failed;
                    self.emit(EngineEvent::VolumeFailed {
                        volume: label,
                        message: e.to_string(),
                    });
                    return;
                }
            };
            if slot.stop.load(Ordering::Relaxed) {
                return;
            }
            let entries = idx.live_len() as u64;
            *slot.scanned.lock() = entries;
            slot.install_index(idx);
            // Scan path: next_usn is the position at journal open (before the
            // scan), so a flush taken now replays the scan window — correct,
            // just slightly redundant.
            *slot.checkpoint.lock() = Some(JournalCheckpoint {
                journal_id: journal.journal_id(),
                next_usn: journal.next_usn(),
            });
            *slot.phase.lock() = VolumeState::Ready;
            self.emit(EngineEvent::VolumeReady {
                volume: label.clone(),
                entries,
            });
            // 2. Tail the journal until stop or a condition requiring a clean
            // rebuild.
            let mut buf = Vec::new();
            // None = "never emitted" → the first change emits immediately. Avoids
            // `Instant - DEBOUNCE` and `checked_sub(..).unwrap()`, both of which
            // panic at boot when uptime < DEBOUNCE.
            let mut last_emit: Option<Instant> = None;
            let begin_rescan = |cause: RescanCause| {
                if cause == RescanCause::MalformedBatch {
                    Counters::bump(&self.metrics.counters.usn_batches_truncated);
                }
                crate::degrade!(
                    self.metrics.counters.journal_rescans,
                    volume = %label,
                    ?cause,
                    "change source invalidated — full rescan"
                );
                // A flush during the rescan must not pair a rejected cursor
                // with either the old index or the rebuilt one.
                *slot.checkpoint.lock() = None;
                *slot.phase.lock() = VolumeState::Rescanning;
                self.emit(EngineEvent::RescanStarted {
                    volume: label.clone(),
                });
                store.remove();
            };
            loop {
                if slot.stop.load(Ordering::Relaxed) {
                    self.save_slot(
                        &slot,
                        JournalCheckpoint {
                            journal_id: journal.journal_id(),
                            next_usn: journal.next_usn(),
                        },
                    );
                    return;
                }
                match journal_gone_action(journal.read_blocking(&mut buf)) {
                    TailStep::Apply(rs) => {
                        if rs.is_empty() {
                            continue;
                        }
                        let mut rescan = None;
                        let stage = crate::metrics::Stage::start();
                        // Resolve the batch's volume reads (complete hard-link
                        // sets, size/mtime) under a *shared* lock. They are
                        // blocking raw-volume round-trips; running them inside
                        // the write guard below would block every concurrent
                        // query for as long as the disk takes, which the
                        // single-digit-millisecond search budget cannot absorb.
                        // This worker is the index's only writer, so nothing
                        // can mutate between the two phases.
                        let Some(plan) = ({
                            let index = slot.index.read();
                            index.as_ref().map(|idx| plan_batch(idx, &rs, &fetch))
                        }) else {
                            begin_rescan(RescanCause::IndexUnavailable);
                            break;
                        };
                        {
                            let mut index = slot.index.write();
                            let Some(idx) = index.as_mut() else {
                                drop(index);
                                begin_rescan(RescanCause::IndexUnavailable);
                                break;
                            };
                            let s = apply_planned(idx, &rs, plan);
                            if s.stat_failures > 0 {
                                crate::degrade!(
                                    self.metrics.counters.stat_fetch_failures,
                                    count = u64::from(s.stat_failures),
                                    volume = %label,
                                    failures = s.stat_failures,
                                    "journal metadata stat fetches failed; prior values were retained"
                                );
                            }
                            if s.hard_link_refresh_failures > 0 {
                                crate::degrade!(
                                    self.metrics.counters.hard_link_refresh_failures,
                                    count = u64::from(s.hard_link_refresh_failures),
                                    volume = %label,
                                    failures = s.hard_link_refresh_failures,
                                    "hard-link refresh failed; journal batch rejected"
                                );
                            }
                            if s.index_rejections > 0 {
                                crate::degrade!(
                                    self.metrics.counters.usn_index_rejections,
                                    count = u64::from(s.index_rejections),
                                    volume = %label,
                                    rejections = s.index_rejections,
                                    "journal records would have broken an index invariant; batch rejected"
                                );
                            }
                            if s.rescan_required {
                                rescan = Some(if s.index_rejections > 0 {
                                    RescanCause::IndexRejected
                                } else {
                                    RescanCause::MetadataRefreshFailed
                                });
                            }
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
                        if let Some(cause) = rescan {
                            begin_rescan(cause);
                            break;
                        }
                        // Concurrency invariant (checkpoint-after-apply):
                        // the index is mutated first, the shared checkpoint
                        // published second. A concurrent `Engine::flush`
                        // reading the checkpoint first therefore always
                        // saves checkpoint ≤ index — the USN replay on load
                        // covers the gap (re-applying records is idempotent;
                        // skipping them would not be — see
                        // `JournalCheckpoint`). Do not reorder these two.
                        *slot.checkpoint.lock() = Some(JournalCheckpoint {
                            journal_id: journal.journal_id(),
                            next_usn: journal.next_usn(),
                        });
                        self.maybe_compact(&slot);
                        if last_emit.is_none_or(|t| t.elapsed() >= INDEX_CHANGED_DEBOUNCE) {
                            last_emit = Some(Instant::now());
                            self.emit(EngineEvent::IndexChanged {
                                volume: label.clone(),
                            });
                        }
                    }
                    TailStep::Rescan(cause) => {
                        begin_rescan(cause);
                        break; // restart the outer loop → fresh journal + scan
                    }
                    TailStep::Fail(e) => {
                        *slot.phase.lock() = VolumeState::Failed;
                        self.emit(EngineEvent::VolumeFailed {
                            volume: label,
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
    pub(super) fn maybe_compact(&self, slot: &VolumeSlot) {
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
        // Generation recheck between copy and swap — pure half in
        // `compact_recheck` (see its doc for the single-writer invariant).
        let guard = slot.index.read();
        let current = guard
            .as_ref()
            .map(crate::index::VolumeIndex::content_generation);
        drop(guard);
        if compact_recheck(compacted.1, current) == CompactionVerdict::Abort {
            crate::degrade!(
                self.metrics.counters.compaction_aborts,
                volume = %slot.label,
                "index mutated during compaction — copy discarded"
            );
            return;
        }
        slot.install_index(compacted.0);
    }
}

/// Test-only spawn with both seams injected (production wiring is
/// `index_start` → `volume_thread`). Mirrors `index_start`'s slot
/// construction and thread naming exactly.
#[cfg(all(test, windows))]
impl Engine {
    pub(super) fn spawn_worker_with_seams(
        self: &Arc<Self>,
        label: &str,
        store: Arc<dyn super::seams::SnapshotStore>,
        mut journal: Box<dyn JournalSource>,
    ) {
        let slot = Arc::new(VolumeSlot::scanning(label.to_string(), store));
        self.volumes.write().push(slot.clone());
        let engine = self.clone();
        let handle = std::thread::Builder::new()
            .name(format!("fmf-vol-{label}"))
            .spawn(move || engine.volume_thread_with(slot, journal.as_mut()))
            .expect("spawn volume thread");
        self.threads.lock().push(handle);
    }
}
