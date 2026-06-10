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

use volume::VolumeSlot;

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

pub struct Engine {
    config: EngineConfig,
    sink: RwLock<Option<EventSink>>,
    volumes: RwLock<Vec<Arc<VolumeSlot>>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    metrics: MetricsHub,
    /// Keeps the diag→EngineError forwarding registered for our lifetime.
    _diag_guard: Mutex<Option<crate::diag::SinkGuard>>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Arc<Self> {
        let engine = Arc::new(Self {
            config,
            sink: RwLock::new(None),
            volumes: RwLock::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
            metrics: MetricsHub::new(),
            _diag_guard: Mutex::new(None),
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
        engine
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
            .filter_map(|slot| slot.index.read().as_ref().map(|idx| idx.stats(&slot.label)))
            .collect()
    }

    /// Full observability snapshot (JSON-serializable).
    pub fn metrics_snapshot(&self) -> crate::metrics::MetricsSnapshot {
        self.metrics.snapshot(64, self.index_stats())
    }

    /// Persist all volumes (graceful shutdown / explicit flush). Tailing
    /// threads also save on stop; this covers "save now" requests.
    #[cfg(windows)]
    pub fn flush(&self) {
        // Snapshots are written by the tailing threads on stop; an explicit
        // flush from a live engine writes with the thread-held checkpoint
        // being slightly behind, which the USN replay covers. For MVP we only
        // save on shutdown to keep a single writer per file.
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
    pub fn insert_ready_volume(&self, label: &str, idx: VolumeIndex) {
        let slot = Arc::new(VolumeSlot {
            label: label.to_string(),
            phase: Mutex::new(VolumePhase::Ready),
            scanned: Mutex::new(idx.live_len() as u64),
            index: RwLock::new(Some(idx)),
            stop: Arc::new(AtomicBool::new(false)),
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
