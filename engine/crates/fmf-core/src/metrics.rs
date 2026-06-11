//! Engine observability: per-operation traces, ring buffers of recent
//! activity, and log2-bucket latency histograms. Everything here is cheap
//! enough to stay on in production — an `Instant` pair and a few integer
//! adds per operation (no allocation on the hot path beyond the trace
//! struct itself).

use std::collections::VecDeque;

use parking_lot::Mutex;
use serde::Serialize;

/// Stage breakdown of one query, in microseconds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct QueryTrace {
    pub query: String,
    /// Which execution strategy drove candidate generation (visualized in
    /// the perf panel): e.g. "full-scan", "pool-scan", "suffix", "perm-walk".
    pub driver: String,
    /// Per-volume query-cache outcome: "miss", "refine" (all volumes
    /// narrowed the previous result) or "partial" (mixed).
    pub cache: String,
    /// True when this query is identical (text + options) to the previous
    /// one on every volume *and* produced identical id lists — the UI keeps
    /// the displayed result instead of re-publishing (no repaint churn from
    /// idle USN traffic).
    pub unchanged: bool,
    pub parse_us: u64,
    pub compile_us: u64,
    /// Dir-path memo (only path queries; 0 when cached/warm).
    pub memo_us: u64,
    pub scan_us: u64,
    pub materialize_us: u64,
    /// Multi-volume k-way merge.
    pub merge_us: u64,
    pub total_us: u64,
    pub entries_scanned: u64,
    pub excluded_skipped: u64,
    pub hits: u64,
    pub volumes: u32,
}

/// One index-established event of a volume: an initial scan/rescan ("scan")
/// or a snapshot restore ("snapshot"). Sharing one timeline keeps the ≤2s
/// restore gate visible next to full-scan costs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanTrace {
    pub volume: String,
    /// "scan" | "snapshot".
    pub source: String,
    pub read_bytes: u64,
    pub read_ms: u64,
    pub mb_per_s: f64,
    pub parse_ms: u64,
    /// Deferred $ATTRIBUTE_LIST name resolution.
    pub deferred_ms: u64,
    pub build_ms: u64,
    pub sort_ms: u64,
    pub total_ms: u64,
    pub entries: u64,
    pub peak_ws_bytes: u64,
}

/// One applied USN batch.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UsnTrace {
    pub volume: String,
    pub records: u64,
    pub upserted: u64,
    pub deleted: u64,
    pub stat_updated: u64,
    pub stat_failures: u64,
    pub apply_us: u64,
}

/// Per-column memory accounting for one volume index.
#[derive(Clone, Debug, Default, Serialize)]
pub struct IndexStats {
    pub volume: String,
    pub entries: u64,
    pub live_entries: u64,
    pub tombstones: u64,
    pub name_pool_bytes: u64,
    pub lower_pool_bytes: u64,
    pub offsets_bytes: u64,
    pub parent_bytes: u64,
    pub size_bytes: u64,
    pub mtime_bytes: u64,
    pub frn_bytes: u64,
    pub flag_bytes: u64,
    pub permutations_bytes: u64,
    pub frn_map_bytes: u64,
    /// Abandoned name bytes across both pools (tombstoned rows, in-place
    /// dir renames: the folded copy always, the original copy when one
    /// existed) — the reclaimable garbage. Compaction-trigger input; a
    /// lower bound right after a snapshot restore.
    pub dead_name_bytes: u64,
    /// `dead_name_bytes / (name_pool + lower_pool)`.
    pub pool_garbage_ratio: f64,
    /// Generation-cached query accelerators (offset table, dir-path memo).
    /// Part of the bytes/entry gate — they live in the engine process.
    pub derived_cache_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_entry: f64,
    pub content_generation: u64,
    pub structural_generation: u64,
}

impl IndexStats {
    /// Fold the derived-cache footprint in (the index module cannot compute
    /// it itself — the cached types belong to the query layer).
    pub fn add_derived_bytes(&mut self, bytes: u64) {
        self.derived_cache_bytes = bytes;
        self.total_bytes += bytes;
        self.bytes_per_entry = if self.entries > 0 {
            self.total_bytes as f64 / self.entries as f64
        } else {
            0.0
        };
    }
}

/// Log2-bucketed microsecond histogram: bucket i counts values in
/// [2^i, 2^(i+1)) µs. 32 buckets cover > an hour.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Histogram {
    pub buckets: Vec<u64>, // length 32
    pub count: u64,
    pub sum_us: u64,
    pub max_us: u64,
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            buckets: vec![0; 32],
            ..Default::default()
        }
    }

    pub fn record(&mut self, us: u64) {
        let b = (64 - us.max(1).leading_zeros() as usize - 1).min(31);
        self.buckets[b] += 1;
        self.count += 1;
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
    }

    /// Approximate percentile (upper bound of the containing bucket).
    pub fn percentile_us(&self, p: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((self.count as f64) * p).ceil() as u64;
        let mut seen = 0;
        for (i, &c) in self.buckets.iter().enumerate() {
            seen += c;
            if seen >= target {
                return 1u64 << (i + 1);
            }
        }
        self.max_us
    }
}

/// Degradation counters — "this happened N times" facts that would
/// otherwise vanish into fallback paths. Zero-cost atomics, always on.
#[derive(Debug, Default)]
pub struct Counters {
    pub stat_fetch_failures: std::sync::atomic::AtomicU64,
    pub usn_batches_truncated: std::sync::atomic::AtomicU64,
    pub snapshot_load_failures: std::sync::atomic::AtomicU64,
    pub snapshot_save_failures: std::sync::atomic::AtomicU64,
    pub deferred_names_unresolved: std::sync::atomic::AtomicU64,
    pub corrupt_mft_records: std::sync::atomic::AtomicU64,
    pub journal_rescans: std::sync::atomic::AtomicU64,
    pub scan_pipeline_fallbacks: std::sync::atomic::AtomicU64,
    /// A compacted copy was discarded because the index mutated under it —
    /// impossible while the volume thread is the only writer; nonzero means
    /// that invariant broke somewhere.
    pub compaction_aborts: std::sync::atomic::AtomicU64,
    /// Pipe server (fmf-service): a frame failed validation and the
    /// connection was dropped.
    pub pipe_malformed_frames: std::sync::atomic::AtomicU64,
    /// Pipe server: a subscriber's bounded event queue overflowed and the
    /// oldest event was dropped.
    pub pipe_events_dropped: std::sync::atomic::AtomicU64,
    /// Pipe server: a client was turned away at the instance cap.
    pub pipe_connections_rejected: std::sync::atomic::AtomicU64,
    /// Scan: the extension-record name cache hit its capacity; remaining
    /// deferred names fall back to per-record disk reads.
    pub deferred_name_cache_overflow: std::sync::atomic::AtomicU64,
    /// Scan: a deferred-name disk read failed (the entry keeps a
    /// placeholder name until the next rescan).
    pub deferred_name_read_failures: std::sync::atomic::AtomicU64,
    /// Pipe server: a result handle was LRU-evicted at the per-connection
    /// cap; its next page fetch answers STALE("evicted").
    pub pipe_results_evicted: std::sync::atomic::AtomicU64,
    /// QueryTrace JSON serialization failed; the response carried an empty
    /// trace (the query itself succeeded).
    pub trace_serialize_failures: std::sync::atomic::AtomicU64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CountersSnapshot {
    pub stat_fetch_failures: u64,
    pub usn_batches_truncated: u64,
    pub snapshot_load_failures: u64,
    pub snapshot_save_failures: u64,
    pub deferred_names_unresolved: u64,
    pub corrupt_mft_records: u64,
    pub journal_rescans: u64,
    pub scan_pipeline_fallbacks: u64,
    pub offset_table_rebuild_fallbacks: u64,
    pub lazy_perm_rebuild_fallbacks: u64,
    pub compaction_aborts: u64,
    pub pipe_malformed_frames: u64,
    pub pipe_events_dropped: u64,
    pub pipe_connections_rejected: u64,
    pub deferred_name_cache_overflow: u64,
    pub deferred_name_read_failures: u64,
    pub pipe_results_evicted: u64,
    pub trace_serialize_failures: u64,
}

/// The query layer has no `MetricsHub` handle (its degradations normally go
/// through the diag ring — see memo.rs), so these counters are process
/// globals folded into every snapshot.
static OFFSET_TABLE_REBUILD_FALLBACKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static LAZY_PERM_REBUILD_FALLBACKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

impl Counters {
    pub fn bump(counter: &std::sync::atomic::AtomicU64) {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn bump_offset_table_rebuild_fallbacks() {
        OFFSET_TABLE_REBUILD_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn bump_lazy_perm_rebuild_fallbacks() {
        LAZY_PERM_REBUILD_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn add(counter: &std::sync::atomic::AtomicU64, n: u64) {
        counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> CountersSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        CountersSnapshot {
            stat_fetch_failures: self.stat_fetch_failures.load(Relaxed),
            usn_batches_truncated: self.usn_batches_truncated.load(Relaxed),
            snapshot_load_failures: self.snapshot_load_failures.load(Relaxed),
            snapshot_save_failures: self.snapshot_save_failures.load(Relaxed),
            deferred_names_unresolved: self.deferred_names_unresolved.load(Relaxed),
            corrupt_mft_records: self.corrupt_mft_records.load(Relaxed),
            journal_rescans: self.journal_rescans.load(Relaxed),
            scan_pipeline_fallbacks: self.scan_pipeline_fallbacks.load(Relaxed),
            offset_table_rebuild_fallbacks: OFFSET_TABLE_REBUILD_FALLBACKS.load(Relaxed),
            lazy_perm_rebuild_fallbacks: LAZY_PERM_REBUILD_FALLBACKS.load(Relaxed),
            compaction_aborts: self.compaction_aborts.load(Relaxed),
            pipe_malformed_frames: self.pipe_malformed_frames.load(Relaxed),
            pipe_events_dropped: self.pipe_events_dropped.load(Relaxed),
            pipe_connections_rejected: self.pipe_connections_rejected.load(Relaxed),
            deferred_name_cache_overflow: self.deferred_name_cache_overflow.load(Relaxed),
            deferred_name_read_failures: self.deferred_name_read_failures.load(Relaxed),
            pipe_results_evicted: self.pipe_results_evicted.load(Relaxed),
            trace_serialize_failures: self.trace_serialize_failures.load(Relaxed),
        }
    }
}

/// Aggregated, JSON-serializable snapshot for the FFI/UI.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MetricsSnapshot {
    pub recent_queries: Vec<QueryTrace>,
    pub query_histogram: Histogram,
    pub p50_us: u64,
    pub p99_us: u64,
    pub recent_usn: Vec<UsnTrace>,
    pub scans: Vec<ScanTrace>,
    pub indexes: Vec<IndexStats>,
    pub counters: CountersSnapshot,
    /// WARN+ events and panics (diag ring), oldest first.
    pub recent_errors: Vec<crate::diag::ErrorEvent>,
}

const RING: usize = 256;
const USN_RING: usize = 64;
const SCAN_RING: usize = 64;

/// Thread-safe metrics collector owned by the engine.
#[derive(Default)]
pub struct MetricsHub {
    queries: Mutex<VecDeque<QueryTrace>>,
    histogram: Mutex<Histogram>,
    usn: Mutex<VecDeque<UsnTrace>>,
    scans: Mutex<VecDeque<ScanTrace>>,
    pub counters: Counters,
}

impl MetricsHub {
    pub fn new() -> Self {
        Self {
            histogram: Mutex::new(Histogram::new()),
            ..Default::default()
        }
    }

    pub fn record_query(&self, trace: QueryTrace) {
        self.histogram.lock().record(trace.total_us);
        let mut q = self.queries.lock();
        if q.len() == RING {
            q.pop_front();
        }
        q.push_back(trace);
    }

    pub fn record_usn(&self, trace: UsnTrace) {
        let mut u = self.usn.lock();
        if u.len() == USN_RING {
            u.pop_front();
        }
        u.push_back(trace);
    }

    pub fn record_scan(&self, trace: ScanTrace) {
        let mut s = self.scans.lock();
        if s.len() == SCAN_RING {
            s.pop_front();
        }
        s.push_back(trace);
    }

    pub fn last_query(&self) -> Option<QueryTrace> {
        self.queries.lock().back().cloned()
    }

    /// Snapshot with the most recent `recent` queries (newest last).
    pub fn snapshot(&self, recent: usize, indexes: Vec<IndexStats>) -> MetricsSnapshot {
        let hist = self.histogram.lock().clone();
        MetricsSnapshot {
            recent_queries: {
                let q = self.queries.lock();
                q.iter().rev().take(recent).rev().cloned().collect()
            },
            p50_us: hist.percentile_us(0.50),
            p99_us: hist.percentile_us(0.99),
            query_histogram: hist,
            recent_usn: self.usn.lock().iter().cloned().collect(),
            scans: self.scans.lock().iter().cloned().collect(),
            indexes,
            counters: self.counters.snapshot(),
            recent_errors: crate::diag::recent_errors(),
        }
    }
}

/// Microsecond stopwatch for stage timing.
pub struct Stage(std::time::Instant);

impl Stage {
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }

    /// Elapsed µs and restart — chain stages with one clock.
    pub fn lap(&mut self) -> u64 {
        let us = self.0.elapsed().as_micros() as u64;
        self.0 = std::time::Instant::now();
        us
    }

    pub fn elapsed_us(&self) -> u64 {
        self.0.elapsed().as_micros() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_and_percentiles() {
        let mut h = Histogram::new();
        for us in [1u64, 2, 3, 100, 1000, 10_000] {
            h.record(us);
        }
        assert_eq!(h.count, 6);
        assert_eq!(h.max_us, 10_000);
        // p50 lands in a small bucket, p99 near the max bucket.
        assert!(h.percentile_us(0.5) <= 8);
        assert!(h.percentile_us(0.99) >= 8192);
    }

    #[test]
    fn ring_buffer_caps() {
        let hub = MetricsHub::new();
        for i in 0..300 {
            hub.record_query(QueryTrace {
                total_us: i,
                ..Default::default()
            });
            hub.record_scan(ScanTrace {
                total_ms: i,
                ..Default::default()
            });
        }
        let snap = hub.snapshot(16, Vec::new());
        assert_eq!(snap.query_histogram.count, 300);
        assert_eq!(snap.recent_queries.len(), 16);
        // Newest last.
        assert_eq!(snap.recent_queries.last().unwrap().total_us, 299);
        // Scans are a ring too — a long-lived process must not grow it.
        assert_eq!(snap.scans.len(), SCAN_RING);
        assert_eq!(snap.scans.last().unwrap().total_ms, 299);
    }
}
