//! Multi-volume engine assembly.
//!
//! Owns one `VolumeIndex` per NTFS volume, drives initial scans and USN
//! tailing threads, and answers queries with a k-way-merged, sort-ordered
//! result set (docs/ARCHITECTURE.md). This is the layer the FFI exposes 1:1
//! — and the layer a v2 service would host.

mod results;
mod seams;
mod search;
mod volume;
#[cfg(windows)]
mod worker;

#[cfg(test)]
mod tests;
#[cfg(all(test, windows))]
mod worker_tests;

pub use results::{ResultSet, Row};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::{Mutex, RwLock};
use thiserror::Error;

use crate::index::VolumeIndex;
use crate::metrics::MetricsHub;
use crate::query;

use volume::{JournalCheckpoint, VolumeSlot};

/// Engine startup configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Root directory holding per-volume snapshots and the `.writer.lock`
    /// (`%ProgramData%\find-my-files\`).
    pub index_dir: PathBuf,
}

// The volume state is contract surface (FmfVolumeStatus.state /
// VolumeStatusWire.state carry it as u32) — the engine uses the canonical
// definition directly, so no wire↔engine mapping exists (ADR-0018).
pub use fmf_contract::options::VolumeState;

/// Asynchronous notification a volume emits to the event sink during scanning
/// and tailing (mapped 1:1 to a contract POD by [`EngineEvent::to_wire`]).
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Initial-scan progress: `entries` files seen so far on `volume`.
    Progress {
        /// Volume label (e.g. `"C:"`).
        volume: String,
        /// Files indexed so far (running count).
        entries: u64,
    },
    /// `volume`'s initial scan finished; it is now queryable with `entries`
    /// total files.
    VolumeReady {
        /// Volume label (e.g. `"C:"`).
        volume: String,
        /// Total files indexed when the scan completed (count).
        entries: u64,
    },
    /// Emitted (debounced, engine-side only throttle) after USN batches.
    IndexChanged {
        /// Volume label (e.g. `"C:"`).
        volume: String,
    },
    /// A full rescan of `volume` has begun (e.g. the USN journal was lost).
    RescanStarted {
        /// Volume label (e.g. `"C:"`).
        volume: String,
    },
    /// `volume` could not be opened or scanned; `message` is the human-readable
    /// reason.
    VolumeFailed {
        /// Volume label (e.g. `"C:"`).
        volume: String,
        /// Human-readable failure reason.
        message: String,
    },
    /// A WARN/ERROR/panic was recorded in the diagnostics ring; the UI pulls
    /// details from the metrics snapshot (push notification + pull detail).
    EngineError {
        /// Severity recorded in the diagnostics ring (1=warn, 2=error,
        /// 3=panic).
        severity: u64, // 1=warn 2=error 3=panic
        /// Volume label the diagnostic was attributed to (empty if none).
        volume: String,
    },
}

impl EngineEvent {
    /// The single `EngineEvent` → contract POD mapping — the FFI callback and
    /// the pipe event push both consume this (ADR-0018: no per-boundary
    /// kind tables).
    #[must_use]
    pub fn to_wire(&self) -> fmf_contract::pod::FmfEvent {
        use fmf_contract::events::EventKind;
        let (kind, volume, entries) = match self {
            Self::Progress { volume, entries } => (EventKind::Progress, volume, *entries),
            Self::VolumeReady { volume, entries } => (EventKind::VolumeReady, volume, *entries),
            Self::IndexChanged { volume } => (EventKind::IndexChanged, volume, 0),
            Self::RescanStarted { volume } => (EventKind::RescanStarted, volume, 0),
            Self::VolumeFailed { volume, .. } => (EventKind::VolumeFailed, volume, 0),
            Self::EngineError { severity, volume } => (EventKind::EngineError, volume, *severity),
        };
        fmf_contract::pod::FmfEvent::new(kind as u32, entries, volume)
    }
}

/// Callback the engine invokes (from any thread) to deliver an [`EngineEvent`].
pub type EventSink = Arc<dyn Fn(&EngineEvent) + Send + Sync>;

/// A failure answering a query (parse, compile, or a stale result set).
#[derive(Debug, Error)]
pub enum EngineError {
    /// The query text could not be parsed.
    #[error("query parse: {0}")]
    Parse(#[from] query::ParseError),
    /// The parsed query could not be compiled.
    #[error("query compile: {0}")]
    Compile(#[from] query::CompileError),
    /// The result set references an index that has since been rebuilt.
    #[error("result is stale (index was rebuilt)")]
    Stale,
}

/// Why `Engine::new` refused to start. `Locked` is the cross-process arm of
/// the single-writer invariant (FFI: `FMF_E_LOCKED`, docs/ARCHITECTURE.md
/// Pipe プロトコル §単一書き手の排他).
#[derive(Debug, Error)]
pub enum EngineCreateError {
    #[error(
        "index directory is locked by another engine process (holder pid: {})",
        .0.map_or_else(|| "unknown".to_string(), |p| p.to_string())
    )]
    /// Another engine process already holds the writer lock (its pid if
    /// readable). The cross-process arm of the single-writer invariant.
    Locked(Option<u32>),
    /// The index directory could not be created or its lock could not be
    /// opened.
    #[error("index directory: {0}")]
    Io(#[from] std::io::Error),
}

/// The multi-volume engine: owns one index per NTFS volume, drives scans and
/// USN tailing, and answers queries. Holds the single-writer lock for its
/// whole lifetime.
pub struct Engine {
    config: EngineConfig,
    sink: RwLock<Option<EventSink>>,
    volumes: RwLock<Vec<Arc<VolumeSlot>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    metrics: MetricsHub,
    /// Last `(text, options) → compiled query`. An identical re-issue — a USN-
    /// driven requery of the same text (the `RefreshInPlace` path) — then skips
    /// parse + compile, which matters most for a heavy regex. Always sound:
    /// the compiled query is a pure function of `(text, options)` (the date
    /// resolver maps civil dates to ticks independently of the wall clock).
    /// Keying on the whole `QueryOptions` over-approximates (only case + the
    /// regex mode/scope actually steer compilation) but stays trivially
    /// correct. Engine-wide because compilation is volume-independent.
    compile_cache: Mutex<Option<(String, query::QueryOptions, Arc<query::CompiledQuery>)>>,
    /// Keeps the diag→EngineError forwarding registered for our lifetime.
    _diag_guard: Mutex<Option<crate::diag::SinkGuard>>,
    /// Exclusive-write handle on `{index_dir}\.writer.lock` for our whole
    /// lifetime — the OS releases it on process death, so no stale locks.
    #[cfg(windows)]
    _writer_lock: std::fs::File,
}

impl Engine {
    /// Create the engine and acquire the single-writer lock on the index
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns [`EngineCreateError::Io`] if the index directory cannot be
    /// created, or [`EngineCreateError::Locked`] if another engine process
    /// already holds the writer lock (`FMF_E_LOCKED`).
    pub fn new(config: EngineConfig) -> Result<Arc<Self>, EngineCreateError> {
        std::fs::create_dir_all(&config.index_dir)?;
        #[cfg(windows)]
        let writer_lock = Self::acquire_writer_lock(&config.index_dir)?;
        let engine = Arc::new(Self {
            config,
            sink: RwLock::new(None),
            volumes: RwLock::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
            metrics: MetricsHub::new(),
            compile_cache: Mutex::new(None),
            _diag_guard: Mutex::new(None),
            #[cfg(windows)]
            _writer_lock: writer_lock,
        });
        // Forward every diagnostics event (WARN+/panic, any thread) to the
        // event sink as a POD EngineError — the UI fetches the message text
        // from the metrics snapshot. Weak: the registry must not keep the
        // engine alive.
        let weak = Arc::downgrade(&engine);
        let guard = crate::diag::register_sink(Arc::new(move |ev| {
            if let Some(e) = weak.upgrade() {
                e.emit(EngineEvent::EngineError {
                    severity: ev.severity.as_u64(),
                    volume: ev.volume.clone().unwrap_or_default(),
                });
            }
        }));
        *engine._diag_guard.lock() = Some(guard);
        Ok(engine)
    }

    /// Cross-process single-writer guard: exclusive write access on
    /// `.writer.lock` (readers allowed, so a losing process can report the
    /// holder's pid). Held until drop; the OS frees it on process death.
    #[cfg(windows)]
    fn acquire_writer_lock(
        index_dir: &std::path::Path,
    ) -> Result<std::fs::File, EngineCreateError> {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const ERROR_SHARING_VIOLATION: i32 = 32;

        let path = index_dir.join(".writer.lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
        {
            Ok(mut f) => {
                // Best effort — the pid is diagnostics for the loser, not state.
                let _ = write!(f, "{}", std::process::id());
                let _ = f.flush();
                Ok(f)
            }
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                let holder = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                Err(EngineCreateError::Locked(holder))
            }
            Err(e) => Err(EngineCreateError::Io(e)),
        }
    }

    /// Install (or clear with `None`) the callback that receives every
    /// [`EngineEvent`].
    pub fn set_event_sink(&self, sink: Option<EventSink>) {
        *self.sink.write() = sink;
    }

    fn emit(&self, ev: EngineEvent) {
        if let Some(s) = self.sink.read().clone() {
            s(&ev);
        }
    }

    /// Begin indexing the given volumes (asynchronous; progress via events).
    /// Idempotent per volume label: clients re-send `IndexStart` on every
    /// (re)connect and the service also calls this at startup, so a volume
    /// already being indexed is skipped. A duplicate slot would make every
    /// query return that volume's rows once per copy (search merges all Ready
    /// slots) — the source of the "each result appears N times" bug.
    ///
    /// # Panics
    ///
    /// Panics if a volume worker thread cannot be spawned.
    pub fn index_start(self: &Arc<Self>, volumes: &[String]) {
        for label in volumes {
            // Trust boundary: `volumes` reaches us unvalidated — over the pipe
            // (IndexStart op), through the FFI, and from service startup. A
            // label must be exactly "<letter>:"; reject anything else here, the
            // one chokepoint every caller funnels through, so a hostile request
            // can neither spawn unbounded volume threads nor steer
            // `snapshot_path` outside the index dir with a `..\` label. Report
            // it as VolumeFailed — the same way the worker surfaces a volume it
            // can't open — so the client still gets feedback (黙らない) and we
            // never reach the slot/thread/`snapshot_path` path with garbage.
            if !volume::is_valid_volume_label(label) {
                tracing::warn!(label = %label, "index_start: rejecting malformed volume label");
                self.emit(EngineEvent::VolumeFailed {
                    volume: label.clone(),
                    message: "malformed volume label (expected \"<letter>:\")".to_string(),
                });
                continue;
            }
            // Decide-and-insert under one write lock so a concurrent
            // index_start of the same label can't slip a second slot in.
            let slot = {
                let mut vols = self.volumes.write();
                if vols.iter().any(|s| s.label == *label) {
                    continue;
                }
                let store = Arc::new(seams::WinSnapshotStore::new(volume::snapshot_path(
                    &self.config.index_dir,
                    label,
                )));
                let slot = Arc::new(VolumeSlot::scanning(label.clone(), store));
                vols.push(slot.clone());
                slot
            };
            let engine = self.clone();
            let handle = std::thread::Builder::new()
                .name(format!("fmf-vol-{label}"))
                .spawn(move || engine.volume_thread(slot))
                .expect("spawn volume thread");
            self.threads.lock().push(handle);
        }
    }

    /// Per-volume status: `(label, state, files scanned so far)`.
    pub fn status(&self) -> Vec<(String, VolumeState, u64)> {
        self.volumes
            .read()
            .iter()
            .map(|s| (s.label.clone(), *s.phase.lock(), *s.scanned.lock()))
            .collect()
    }

    /// The engine's metrics hub (counters and the diagnostics ring).
    pub const fn metrics(&self) -> &MetricsHub {
        &self.metrics
    }

    /// Per-volume memory accounting (perf panel / `fmf stats`).
    pub fn index_stats(&self) -> Vec<crate::metrics::IndexStats> {
        self.volumes
            .read()
            .iter()
            .filter_map(|slot| {
                slot.index.read().as_ref().map(|idx| {
                    let mut s = idx.stats(&slot.label);
                    s.add_derived_bytes(query::derived_cache_bytes(idx));
                    s
                })
            })
            .collect()
    }

    /// Full observability snapshot (JSON-serializable).
    pub fn metrics_snapshot(&self) -> crate::metrics::MetricsSnapshot {
        self.metrics.snapshot(64, self.index_stats())
    }

    /// Persist every Ready volume whose generations moved since its last
    /// save ("dirty"), using the tailing thread's shared checkpoint. The
    /// checkpoint may trail the index by an in-flight batch — the USN
    /// replay on load covers that. Returns the number of snapshots written
    /// (failed writes are counted in `snapshot_save_failures` and excluded).
    #[cfg(windows)]
    pub fn flush(&self) -> usize {
        let volumes: Vec<_> = self.volumes.read().clone();
        let mut saved = 0;
        for slot in volumes {
            if *slot.phase.lock() != VolumeState::Ready {
                continue;
            }
            // Checkpoint before index: a batch landing in between leaves the
            // index newer than the checkpoint, never older.
            let Some(ckpt) = *slot.checkpoint.lock() else {
                continue;
            };
            let dirty = {
                let guard = slot.index.read();
                let Some(idx) = guard.as_ref() else { continue };
                *slot.last_saved.lock()
                    != Some((idx.content_generation(), idx.structural_generation()))
            };
            if !dirty {
                continue;
            }
            if self.save_slot(&slot, ckpt) {
                saved += 1;
            }
        }
        saved
    }

    /// Signal every volume thread to stop and join them (bounded wait).
    pub fn shutdown(&self) {
        // Close the diag→EngineError forwarding window first: shutdown-time
        // WARNs (final flush, journal teardown) still reach the log and the
        // diag ring, but no longer race the dying event sink.
        *self._diag_guard.lock() = None;
        for slot in self.volumes.read().iter() {
            slot.stop.store(true, Ordering::Relaxed);
        }
        // Blocked journal reads return on the next volume write; joining with
        // a bounded wait keeps shutdown prompt without CancelSynchronousIo
        // (M2 refinement).
        let mut threads = self.threads.lock();
        for t in threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Test/dev helper: register an already-built index as a Ready volume.
    /// The zero checkpoint stands in for a journal position so `flush` can
    /// exercise the save path on injected volumes.
    pub fn insert_ready_volume(&self, label: &str, idx: VolumeIndex) {
        let slot = Arc::new(VolumeSlot {
            label: label.to_string(),
            phase: Mutex::new(VolumeState::Ready),
            scanned: Mutex::new(idx.live_len() as u64),
            index: RwLock::new(Some(idx)),
            stop: Arc::new(AtomicBool::new(false)),
            last_query: Mutex::new(None),
            checkpoint: Mutex::new(Some(JournalCheckpoint {
                journal_id: 0,
                next_usn: 0,
            })),
            last_saved: Mutex::new(None),
            save_lock: Mutex::new(()),
            store: Arc::new(seams::WinSnapshotStore::new(volume::snapshot_path(
                &self.config.index_dir,
                label,
            ))),
        });
        self.volumes.write().push(slot);
    }

    /// Test/dev helper: swap a rebuilt index into an existing Ready volume —
    /// the same structural replacement a journal-gone full rescan performs.
    ///
    /// # Panics
    ///
    /// Panics if no volume with the given `label` exists.
    pub fn replace_ready_volume(&self, label: &str, idx: VolumeIndex) {
        let volumes = self.volumes.read();
        let slot = volumes
            .iter()
            .find(|s| s.label == label)
            .expect("replace_ready_volume: unknown volume");
        *slot.scanned.lock() = idx.live_len() as u64;
        slot.install_index(idx);
    }
}
