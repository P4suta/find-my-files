//! Engine observability: per-operation traces, ring buffers of recent
//! activity, and log2-bucket latency histograms.
//!
//! Everything here is cheap enough to stay on in production — an `Instant`
//! pair and a few integer adds per operation (no allocation on the hot path
//! beyond the trace struct itself).

use std::collections::VecDeque;

use parking_lot::Mutex;
use serde::Serialize;

/// Stage breakdown of one query, in microseconds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct QueryTrace {
    /// The raw query text this trace measured.
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
    /// Query parse time, in microseconds.
    pub parse_us: u64,
    /// Query compile time, in microseconds.
    pub compile_us: u64,
    /// Dir-path memo (only path queries; 0 when cached/warm).
    pub memo_us: u64,
    /// Candidate-generation scan time, in microseconds.
    pub scan_us: u64,
    /// Result-row materialization time, in microseconds.
    pub materialize_us: u64,
    /// Multi-volume k-way merge.
    pub merge_us: u64,
    /// End-to-end query time, in microseconds.
    pub total_us: u64,
    /// Number of index entries examined during scanning.
    pub entries_scanned: u64,
    /// Number of entries skipped by exclusion rules.
    pub excluded_skipped: u64,
    /// Number of matching entries returned.
    pub hits: u64,
    /// Number of volumes queried.
    pub volumes: u32,
}

/// One index-established event of a volume: an initial scan/rescan ("scan")
/// or a snapshot restore ("snapshot"). Sharing one timeline keeps the ≤2s
/// restore gate visible next to full-scan costs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanTrace {
    /// The volume this event established (e.g. "C:").
    pub volume: String,
    /// "scan" | "snapshot".
    pub source: String,
    /// Bytes read from the MFT / snapshot.
    pub read_bytes: u64,
    /// Raw read time, in milliseconds.
    pub read_ms: u64,
    /// Read throughput, in megabytes per second.
    pub mb_per_s: f64,
    /// MFT-record parse time, in milliseconds.
    pub parse_ms: u64,
    /// Deferred $`ATTRIBUTE_LIST` name resolution.
    pub deferred_ms: u64,
    /// Index-build time, in milliseconds.
    pub build_ms: u64,
    /// Sort time, in milliseconds.
    pub sort_ms: u64,
    /// End-to-end time to establish the index, in milliseconds.
    pub total_ms: u64,
    /// Number of index entries established.
    pub entries: u64,
    /// Peak process working set during the event, in bytes.
    pub peak_ws_bytes: u64,
}

/// One applied USN batch.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UsnTrace {
    /// The volume this USN batch was applied to (e.g. "C:").
    pub volume: String,
    /// Number of USN records in the batch.
    pub records: u64,
    /// Number of entries inserted or updated.
    pub upserted: u64,
    /// Number of entries removed (tombstoned).
    pub deleted: u64,
    /// Number of entries whose size/mtime stat was refreshed.
    pub stat_updated: u64,
    /// Number of stat refreshes that failed.
    pub stat_failures: u64,
    /// Time to apply the batch to the index, in microseconds.
    pub apply_us: u64,
}

/// Per-column memory accounting for one volume index.
#[derive(Clone, Debug, Default, Serialize)]
pub struct IndexStats {
    /// The volume this index covers (e.g. "C:").
    pub volume: String,
    /// Total rows in the index (live + tombstones).
    pub entries: u64,
    /// Number of live (non-tombstoned) rows.
    pub live_entries: u64,
    /// Number of tombstoned (deleted) rows.
    pub tombstones: u64,
    /// Bytes held by the original-case name pool.
    pub name_pool_bytes: u64,
    /// Bytes held by the case-folded (lowercase) name pool.
    pub lower_pool_bytes: u64,
    /// Bytes held by the name-pool offset table.
    pub offsets_bytes: u64,
    /// Bytes held by the parent-pointer column.
    pub parent_bytes: u64,
    /// Bytes held by the file-size column.
    pub size_bytes: u64,
    /// Bytes held by the modification-time column.
    pub mtime_bytes: u64,
    /// Bytes held by the File Reference Number column.
    pub frn_bytes: u64,
    /// Bytes held by the per-entry flag column.
    pub flag_bytes: u64,
    /// Bytes held by the sort permutations.
    pub permutations_bytes: u64,
    /// Bytes held by the FRN-to-row lookup map.
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
    /// Total resident bytes for this index (sum of all columns + caches).
    pub total_bytes: u64,
    /// `total_bytes / entries` — the bytes/entry gate metric.
    pub bytes_per_entry: f64,
    /// Content generation counter (bumps on name/data changes).
    pub content_generation: u64,
    /// Structural generation counter (bumps on add/remove/rename).
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
    /// Per-bucket counts; bucket i covers [2^i, 2^(i+1)) µs (length 32).
    pub buckets: Vec<u64>, // length 32
    /// Total number of recorded values.
    pub count: u64,
    /// Sum of all recorded values, in microseconds.
    pub sum_us: u64,
    /// Largest recorded value, in microseconds.
    pub max_us: u64,
}

impl Histogram {
    /// Create an empty histogram with 32 zeroed buckets.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buckets: vec![0; 32],
            ..Default::default()
        }
    }

    /// Record a value (in microseconds) into its log2 bucket and update totals.
    pub fn record(&mut self, us: u64) {
        let b = (64 - us.max(1).leading_zeros() as usize - 1).min(31);
        self.buckets[b] += 1;
        self.count += 1;
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
    }

    /// Approximate percentile (upper bound of the containing bucket).
    #[must_use]
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
    /// Times a per-entry size/mtime stat fetch failed.
    pub stat_fetch_failures: std::sync::atomic::AtomicU64,
    /// Times a USN batch was truncated (records dropped before apply).
    pub usn_batches_truncated: std::sync::atomic::AtomicU64,
    /// Times a snapshot failed to load (fell back to a full scan).
    pub snapshot_load_failures: std::sync::atomic::AtomicU64,
    /// Times a snapshot failed to save.
    pub snapshot_save_failures: std::sync::atomic::AtomicU64,
    /// Times a deferred $`ATTRIBUTE_LIST` name could not be resolved.
    pub deferred_names_unresolved: std::sync::atomic::AtomicU64,
    /// Times a corrupt MFT record was encountered and skipped.
    pub corrupt_mft_records: std::sync::atomic::AtomicU64,
    /// Times the USN journal was rescanned from scratch (gap recovery).
    pub journal_rescans: std::sync::atomic::AtomicU64,
    /// Times the scan pipeline fell back to a slower path.
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
    /// `QueryTrace` JSON serialization failed; the response carried an empty
    /// trace (the query itself succeeded).
    pub trace_serialize_failures: std::sync::atomic::AtomicU64,
    /// Scope walk (ADR-0024): paths skipped because they could not be read
    /// (permission denied or vanished mid-walk) — silent data loss otherwise.
    pub walk_read_errors: std::sync::atomic::AtomicU64,
    /// Scope walk: subtrees not descended because they hit the depth cap.
    pub walk_depth_truncated: std::sync::atomic::AtomicU64,
}

/// Plain-integer, JSON-serializable copy of `Counters` for the FFI/UI.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CountersSnapshot {
    /// Times a per-entry size/mtime stat fetch failed.
    pub stat_fetch_failures: u64,
    /// Times a USN batch was truncated (records dropped before apply).
    pub usn_batches_truncated: u64,
    /// Times a snapshot failed to load (fell back to a full scan).
    pub snapshot_load_failures: u64,
    /// Times a snapshot failed to save.
    pub snapshot_save_failures: u64,
    /// Times a deferred $`ATTRIBUTE_LIST` name could not be resolved.
    pub deferred_names_unresolved: u64,
    /// Times a corrupt MFT record was encountered and skipped.
    pub corrupt_mft_records: u64,
    /// Times the USN journal was rescanned from scratch (gap recovery).
    pub journal_rescans: u64,
    /// Times the scan pipeline fell back to a slower path.
    pub scan_pipeline_fallbacks: u64,
    /// Retired: ADR-0032 removed the query-layer offset table (the name
    /// dictionary is self-indexing). Held at 0 for counter-list stability —
    /// counters are append-only and never removed (see fmf-contract).
    pub offset_table_rebuild_fallbacks: u64,
    /// Times a lazy permutation had to be rebuilt as a fallback.
    pub lazy_perm_rebuild_fallbacks: u64,
    /// A compacted copy was discarded because the index mutated under it —
    /// impossible while the volume thread is the only writer; nonzero means
    /// that invariant broke somewhere.
    pub compaction_aborts: u64,
    /// Pipe server (fmf-service): a frame failed validation and the
    /// connection was dropped.
    pub pipe_malformed_frames: u64,
    /// Pipe server: a subscriber's bounded event queue overflowed and the
    /// oldest event was dropped.
    pub pipe_events_dropped: u64,
    /// Pipe server: a client was turned away at the instance cap.
    pub pipe_connections_rejected: u64,
    /// Scan: the extension-record name cache hit its capacity; remaining
    /// deferred names fall back to per-record disk reads.
    pub deferred_name_cache_overflow: u64,
    /// Scan: a deferred-name disk read failed (the entry keeps a
    /// placeholder name until the next rescan).
    pub deferred_name_read_failures: u64,
    /// Pipe server: a result handle was LRU-evicted at the per-connection
    /// cap; its next page fetch answers STALE("evicted").
    pub pipe_results_evicted: u64,
    /// `QueryTrace` JSON serialization failed; the response carried an empty
    /// trace (the query itself succeeded).
    pub trace_serialize_failures: u64,
    /// Scope walk (ADR-0024): paths skipped because they could not be read.
    pub walk_read_errors: u64,
    /// Scope walk: subtrees not descended because they hit the depth cap.
    pub walk_depth_truncated: u64,
}

/// The query layer has no `MetricsHub` handle (its degradations normally go
/// through the diag ring — see memo.rs), so these counters are process
/// globals folded into every snapshot.
static OFFSET_TABLE_REBUILD_FALLBACKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static LAZY_PERM_REBUILD_FALLBACKS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

impl Counters {
    /// Increment a counter by one (relaxed atomic).
    pub fn bump(counter: &std::sync::atomic::AtomicU64) {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn bump_lazy_perm_rebuild_fallbacks() {
        LAZY_PERM_REBUILD_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Add `n` to a counter (relaxed atomic).
    pub fn add(counter: &std::sync::atomic::AtomicU64, n: u64) {
        counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read all counters into a plain-integer `CountersSnapshot`.
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
            walk_read_errors: self.walk_read_errors.load(Relaxed),
            walk_depth_truncated: self.walk_depth_truncated.load(Relaxed),
        }
    }
}

/// Aggregated, JSON-serializable snapshot for the FFI/UI.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MetricsSnapshot {
    /// Most recent query traces, newest last.
    pub recent_queries: Vec<QueryTrace>,
    /// Latency histogram across all recorded queries.
    pub query_histogram: Histogram,
    /// 50th-percentile query latency, in microseconds.
    pub p50_us: u64,
    /// 99th-percentile query latency, in microseconds.
    pub p99_us: u64,
    /// Most recent applied USN batches.
    pub recent_usn: Vec<UsnTrace>,
    /// Most recent index-established (scan/snapshot) events.
    pub scans: Vec<ScanTrace>,
    /// Per-volume memory accounting for each live index.
    pub indexes: Vec<IndexStats>,
    /// Process-wide degradation counters.
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
    /// Process-wide degradation counters.
    pub counters: Counters,
}

impl MetricsHub {
    /// Create an empty hub with a fresh 32-bucket histogram.
    #[must_use]
    pub fn new() -> Self {
        Self {
            histogram: Mutex::new(Histogram::new()),
            ..Default::default()
        }
    }

    /// Record a query trace into the ring and its latency into the histogram.
    pub fn record_query(&self, trace: QueryTrace) {
        self.histogram.lock().record(trace.total_us);
        let mut q = self.queries.lock();
        if q.len() == RING {
            q.pop_front();
        }
        q.push_back(trace);
    }

    /// Record a USN-batch trace into the ring.
    pub fn record_usn(&self, trace: UsnTrace) {
        let mut u = self.usn.lock();
        if u.len() == USN_RING {
            u.pop_front();
        }
        u.push_back(trace);
    }

    /// Record an index-established (scan/snapshot) trace into the ring.
    pub fn record_scan(&self, trace: ScanTrace) {
        let mut s = self.scans.lock();
        if s.len() == SCAN_RING {
            s.pop_front();
        }
        s.push_back(trace);
    }

    /// The most recently recorded query trace, if any.
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
    /// Start a stopwatch at the current instant.
    #[must_use]
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }

    /// Elapsed µs and restart — chain stages with one clock.
    pub fn lap(&mut self) -> u64 {
        let us = self.0.elapsed().as_micros() as u64;
        self.0 = std::time::Instant::now();
        us
    }

    /// Elapsed time since start (or last lap), in microseconds.
    #[must_use]
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
