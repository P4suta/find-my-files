//! Shared support for the `bench` command: the fixed query set, the report
//! shapes (= the baseline JSON in/out format), percentile helpers and the
//! snapshot restore scenario. Measurement discipline is pinned by ADR-0013 —
//! any logic change here invalidates every recorded baseline, so this module
//! is collect-and-relocate only.

use std::time::Instant;

use fmf_core::index::VolumeIndex;

pub const BENCH_QUERIES: &[&str] = &[
    "",                         // match-all (engine capability; the UI keeps an empty box blank)
    "e",                        // 1 char, huge hit count
    "a",                        // 1 char, huge hit count
    "win",                      // common 3-char substring
    "Win",                      // smart case w/ uppercase: original-name verification path
    "qzx",                      // rare substring
    "ext:dll",                  // extension filter
    "size:>100mb path:windows", // composite
    "*.rs",                     // wildcard
    // Regex with a literal → the prefilter keeps it on the pool sweep, so it
    // honors the p99 budget like any indexed query (ADR-0023).
    "regex:win.*\\.dll",
    // NOTE: a literal-less regex (e.g. `[0-9]{6,}`) has no literal to prefilter
    // on, so it is a full scan whose cost scales linearly with entry count —
    // ~29 ms @1M (within budget) but past the fixed 50 ms line on volumes well
    // over the 1M spec scale. It is measured in the criterion micro-bench
    // (query/regex_scan, ungated) rather than gated here, where the absolute
    // budget would fail purely because the machine holds more files than spec
    // (ADR-0023). Indexing regex is rejected by ADR-0001/0002.
];

#[derive(serde::Serialize, serde::Deserialize)]
pub struct QueryBench {
    pub query: String,
    pub hits: u64,
    pub p50_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub p50_memo_us: u64,
    pub p50_scan_us: u64,
    pub p50_materialize_us: u64,
    /// First iteration of the run — the only one that pays cold derived-cache
    /// builds (memo/offset-table). Single sample: recorded, never gated.
    #[serde(default)]
    pub cold_us: u64,
}

/// Snapshot save/restore timings (page-cache warm: the reproducible
/// CPU-bound part of the ≤2s restore gate; cold I/O is not benchable
/// without admin cache-purge APIs and is too noisy anyway).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RestoreBench {
    pub file_bytes: u64,
    pub entries: u64,
    pub save_ms: u64,
    pub p50_ms: u64,
    pub min_ms: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct BenchReport {
    pub volume: String,
    pub entries: u64,
    pub peak_working_set_bytes: u64,
    pub queries: Vec<QueryBench>,
    /// Absent in baselines recorded before the restore scenario existed.
    #[serde(default)]
    pub restore: Option<RestoreBench>,
}

pub fn median(mut v: Vec<u64>) -> u64 {
    // Defensive only — every caller passes a fixed RUNS-sized vector.
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// Removes the bench's temporary snapshot on every exit path — the `?`
/// returns in [`bench_restore`] used to leak it, and the old `remove_file`
/// failure was silent.
struct TempSnapshotGuard(std::path::PathBuf);

impl Drop for TempSnapshotGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) {
            tracing::warn!(
                "failed to remove bench temp snapshot {}: {e}",
                self.0.display()
            );
        }
    }
}

/// Save the freshly built index to a temp snapshot and measure restores.
/// Page-cache-warm by design: reproducible CPU-bound numbers for the
/// restore→ready gate's deserialization + `frn_map` rebuild share.
pub fn bench_restore(idx: &VolumeIndex) -> Result<RestoreBench, Box<dyn std::error::Error>> {
    const RUNS: usize = 10;
    let temp = std::env::temp_dir().join(format!("fmf-bench-{}.fmfidx", std::process::id()));
    let t = Instant::now();
    idx.save_to(&temp, 0, 0)?;
    let save_ms = t.elapsed().as_millis() as u64;
    let _guard = TempSnapshotGuard(temp.clone());
    let file_bytes = std::fs::metadata(&temp)?.len();

    let mut runs = Vec::with_capacity(RUNS);
    let mut entries = 0u64;
    for _ in 0..RUNS {
        let t = Instant::now();
        let (loaded, _, _) = VolumeIndex::load_from(&temp)?;
        runs.push(t.elapsed().as_millis() as u64);
        entries = loaded.len() as u64;
    }
    runs.sort_unstable();
    Ok(RestoreBench {
        file_bytes,
        entries,
        save_ms,
        p50_ms: runs[RUNS / 2],
        min_ms: runs[0],
    })
}
