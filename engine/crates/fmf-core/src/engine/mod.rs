//! Multi-volume engine assembly: owns one `VolumeIndex` per NTFS volume,
//! drives initial scans and USN tailing threads, and answers queries with a
//! k-way-merged, sort-ordered result set (docs/ARCHITECTURE.md). This is the
//! layer the FFI exposes 1:1 — and the layer a v2 service would host.

mod results;
mod search;
mod volume;

#[cfg(test)]
mod tests;

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

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub index_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumePhase {
    Scanning,
    Ready,
    Rescanning,
    Failed,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Progress {
        volume: String,
        entries: u64,
    },
    VolumeReady {
        volume: String,
        entries: u64,
    },
    /// Emitted (debounced, engine-side only throttle) after USN batches.
    IndexChanged {
        volume: String,
    },
    RescanStarted {
        volume: String,
    },
    VolumeFailed {
        volume: String,
        message: String,
    },
    /// A WARN/ERROR/panic was recorded in the diagnostics ring; the UI pulls
    /// details from the metrics snapshot (push notification + pull detail).
    EngineError {
        severity: u64, // 1=warn 2=error 3=panic
        volume: String,
    },
}

pub type EventSink = Arc<dyn Fn(&EngineEvent) + Send + Sync>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("query parse: {0}")]
    Parse(#[from] query::ParseError),
    #[error("query compile: {0}")]
    Compile(#[from] query::CompileError),
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
    Locked(Option<u32>),
    #[error("index directory: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Engine {
    config: EngineConfig,
    sink: RwLock<Option<EventSink>>,
    volumes: RwLock<Vec<Arc<VolumeSlot>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    metrics: MetricsHub,
    /// Keeps the diag→EngineError forwarding registered for our lifetime.
    _diag_guard: Mutex<Option<crate::diag::SinkGuard>>,
    /// Exclusive-write handle on `{index_dir}\.writer.lock` for our whole
    /// lifetime — the OS releases it on process death, so no stale locks.
    #[cfg(windows)]
    _writer_lock: std::fs::File,
}

impl Engine {
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

    pub fn set_event_sink(&self, sink: Option<EventSink>) {
        *self.sink.write() = sink;
    }

    fn emit(&self, ev: EngineEvent) {
        if let Some(s) = self.sink.read().clone() {
            s(&ev);
        }
    }

    /// Begin indexing the given volumes (asynchronous; progress via events).
    pub fn index_start(self: &Arc<Self>, volumes: &[String]) {
        for label in volumes {
            let slot = Arc::new(VolumeSlot {
                label: label.clone(),
                phase: Mutex::new(VolumePhase::Scanning),
                scanned: Mutex::new(0),
                index: RwLock::new(None),
                stop: Arc::new(AtomicBool::new(false)),
                last_query: Mutex::new(None),
                checkpoint: Mutex::new(None),
                last_saved: Mutex::new(None),
                save_lock: Mutex::new(()),
            });
            self.volumes.write().push(slot.clone());
            let engine = self.clone();
            let handle = std::thread::Builder::new()
                .name(format!("fmf-vol-{label}"))
                .spawn(move || engine.volume_thread(slot))
                .expect("spawn volume thread");
            self.threads.lock().push(handle);
        }
    }

    pub fn status(&self) -> Vec<(String, VolumePhase, u64)> {
        self.volumes
            .read()
            .iter()
            .map(|s| (s.label.clone(), *s.phase.lock(), *s.scanned.lock()))
            .collect()
    }

    pub fn metrics(&self) -> &MetricsHub {
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
            if *slot.phase.lock() != VolumePhase::Ready {
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
            let path = volume::snapshot_path(&self.config.index_dir, &slot.label);
            if self.save_slot(&slot, ckpt, &path) {
                saved += 1;
            }
        }
        saved
    }

    pub fn shutdown(&self) {
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
            phase: Mutex::new(VolumePhase::Ready),
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
        });
        self.volumes.write().push(slot);
    }

    /// Test/dev helper: swap a rebuilt index into an existing Ready volume —
    /// the same structural replacement a journal-gone full rescan performs.
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
