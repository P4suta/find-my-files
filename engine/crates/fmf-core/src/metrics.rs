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

/// One initial-scan (or rescan) of a volume.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanTrace {
    pub volume: String,
    pub read_bytes: u64,
    pub read_ms: u64,
    pub mb_per_s: f64,
    pub parse_ms: u64,
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
    pub total_bytes: u64,
    pub bytes_per_entry: f64,
    pub content_generation: u64,
    pub structural_generation: u64,
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
}

const RING: usize = 256;
const USN_RING: usize = 64;

/// Thread-safe metrics collector owned by the engine.
#[derive(Default)]
pub struct MetricsHub {
    queries: Mutex<VecDeque<QueryTrace>>,
    histogram: Mutex<Histogram>,
    usn: Mutex<VecDeque<UsnTrace>>,
    scans: Mutex<Vec<ScanTrace>>,
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
        self.scans.lock().push(trace);
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
            scans: self.scans.lock().clone(),
            indexes,
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
        }
        let snap = hub.snapshot(16, Vec::new());
        assert_eq!(snap.query_histogram.count, 300);
        assert_eq!(snap.recent_queries.len(), 16);
        // Newest last.
        assert_eq!(snap.recent_queries.last().unwrap().total_us, 299);
    }
}
