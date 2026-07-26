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
    /// First iteration of the run. Single sample: recorded for diagnosis, never
    /// gated (the ready-state memory capture prewarms fixed derived caches).
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
    /// Wall time for the real-volume initial index build.
    #[serde(default)]
    pub index_ms: u64,
    /// Ready-state process working set, captured after query caches are prewarmed.
    #[serde(default)]
    pub working_set_bytes: u64,
    pub peak_working_set_bytes: u64,
    pub queries: Vec<QueryBench>,
    /// Absent in baselines recorded before the restore scenario existed.
    #[serde(default)]
    pub restore: Option<RestoreBench>,
}

/// Initial-index acceptance budget: 8 s at/below 250k entries, then linearly
/// relax to 60 s at 1M (and continue with that slope for larger real volumes).
#[must_use]
pub const fn index_budget_ms(entries: u64) -> u64 {
    const BASE_ENTRIES: u64 = 250_000;
    const BASE_MS: u64 = 8_000;
    const EXTRA_ENTRIES: u64 = 750_000;
    const EXTRA_MS: u64 = 52_000;
    if entries <= BASE_ENTRIES {
        BASE_MS
    } else {
        BASE_MS.saturating_add(
            entries
                .saturating_sub(BASE_ENTRIES)
                .saturating_mul(EXTRA_MS)
                / EXTRA_ENTRIES,
        )
    }
}

/// Ready-state process RAM acceptance line (ADR-0013 / project performance
/// target), expressed without floating-point rounding.
#[must_use]
pub const fn working_set_within_budget(report: &BenchReport) -> bool {
    report.entries == 0 || report.working_set_bytes <= report.entries.saturating_mul(110)
}

/// Whether a real-volume run is still comparable with its recorded baseline.
///
/// A larger corpus can make every relative timing look slower even when the
/// implementation is unchanged, while a smaller one can hide a regression.
/// The ±10% boundary is inclusive; anything beyond it requires an explicit
/// baseline refresh before a verdict is meaningful.
#[must_use]
pub const fn entry_count_within_baseline(entries: u64, baseline_entries: u64) -> bool {
    entries.abs_diff(baseline_entries) <= baseline_entries / 10
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

#[cfg(test)]
mod tests {
    use super::*;

    fn report(entries: u64, working_set_bytes: u64) -> BenchReport {
        BenchReport {
            volume: "C:".to_owned(),
            entries,
            index_ms: 0,
            working_set_bytes,
            peak_working_set_bytes: 0,
            queries: Vec::new(),
            restore: None,
        }
    }

    #[test]
    fn index_budget_pins_acceptance_points() {
        assert_eq!(index_budget_ms(0), 8_000);
        assert_eq!(index_budget_ms(250_000), 8_000);
        assert_eq!(index_budget_ms(1_000_000), 60_000);
        assert_eq!(index_budget_ms(1_750_000), 112_000);
    }

    #[test]
    fn working_set_gate_is_exactly_110_bytes_per_entry() {
        assert!(working_set_within_budget(&report(1_000_000, 110_000_000)));
        assert!(!working_set_within_budget(&report(1_000_000, 110_000_001)));
        assert!(working_set_within_budget(&report(0, u64::MAX)));
    }

    #[test]
    fn entry_count_drift_is_fail_closed_past_ten_percent() {
        assert!(entry_count_within_baseline(900_000, 1_000_000));
        assert!(entry_count_within_baseline(1_100_000, 1_000_000));
        assert!(!entry_count_within_baseline(899_999, 1_000_000));
        assert!(!entry_count_within_baseline(1_100_001, 1_000_000));
        assert!(entry_count_within_baseline(0, 0));
        assert!(!entry_count_within_baseline(1, 0));
    }
}
