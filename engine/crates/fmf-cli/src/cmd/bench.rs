//! `bench` — the fixed real-volume benchmark set + the baseline regression
//! gate (discipline: ADR-0013, engine/benches/README.md).

use std::time::Instant;

use fmf_core::query::QueryOptions;

use super::ctx::Ctx;
use super::{build_index, run_query, term};
use crate::bench_support::{BENCH_QUERIES, BenchReport, QueryBench, bench_restore, median};

pub fn bench(
    drive: &str,
    json: Option<&std::path::Path>,
    baseline: Option<&std::path::Path>,
    ctx: Ctx,
) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive, ctx)?;
    let opt = QueryOptions::default();

    let mut report = BenchReport {
        volume: drive.to_string(),
        entries: idx.len() as u64,
        peak_working_set_bytes: 0,
        queries: Vec::new(),
        restore: None,
    };

    anstream::println!(
        "{}",
        term::paint(
            term::HEADER,
            &format!(
                "{:<28} {:>10} {:>9} {:>9} {:>9} {:>9} | {:>8} {:>8} {:>8}",
                "query", "hits", "p50_us", "p99_us", "max_us", "cold_us", "memo", "scan", "mat"
            )
        )
    );
    for q in BENCH_QUERIES {
        // 200 runs make p99 a real percentile, not the max (ADR-0013).
        const RUNS: usize = 200;
        let mut totals = Vec::with_capacity(RUNS);
        let (mut memos, mut scans, mut mats) = (Vec::new(), Vec::new(), Vec::new());
        let mut hits = 0u64;
        for _ in 0..RUNS {
            let t = Instant::now();
            let (r, m) = run_query(&idx, q, opt)?;
            totals.push(t.elapsed().as_micros() as u64);
            memos.push(m.memo_us);
            scans.push(m.scan_us);
            mats.push(m.materialize_us);
            hits = r.ids.len() as u64;
        }
        let cold_us = totals[0];
        totals.sort_unstable();
        let qb = QueryBench {
            query: q.to_string(),
            hits,
            p50_us: totals[RUNS / 2],
            p99_us: totals[RUNS * 99 / 100],
            max_us: totals[RUNS - 1],
            p50_memo_us: median(memos),
            p50_scan_us: median(scans),
            p50_materialize_us: median(mats),
            cold_us,
        };
        println!(
            "{:<28} {:>10} {:>9} {:>9} {:>9} {:>9} | {:>8} {:>8} {:>8}",
            qb.query,
            qb.hits,
            qb.p50_us,
            qb.p99_us,
            qb.max_us,
            qb.cold_us,
            qb.p50_memo_us,
            qb.p50_scan_us,
            qb.p50_materialize_us
        );
        report.queries.push(qb);
    }
    report.peak_working_set_bytes = fmf_core::mft::peak_working_set();
    println!(
        "peak working set {:.1} MiB",
        report.peak_working_set_bytes as f64 / (1024.0 * 1024.0)
    );

    report.restore = Some(bench_restore(&idx)?);
    if let Some(r) = &report.restore {
        println!(
            "snapshot save {} ms; restore p50 {} ms / min {} ms ({:.1} MiB, {} entries)",
            r.save_ms,
            r.p50_ms,
            r.min_ms,
            r.file_bytes as f64 / (1024.0 * 1024.0),
            r.entries
        );
    }

    if let Some(path) = json {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        eprintln!("report written to {}", path.display());
    }

    if let Some(path) = baseline {
        // Tail latency and restore are gated on *absolute* acceptance
        // budgets, never relative (ADR-0013).
        const P99_BUDGET_US: u64 = 50_000;
        const RESTORE_BUDGET_MS: u64 = 1_000;
        let old: BenchReport = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        // Entry-count drift past ±10% invalidates the baseline (ADR-0013).
        if report.entries.abs_diff(old.entries) > old.entries / 10 {
            anstream::eprintln!(
                "{} entries drifted {}→{} (>10%) since the baseline was recorded — \
                 regression verdicts are unreliable; consider `just bench-baseline`",
                term::paint(term::WARN, "WARNING"),
                old.entries,
                report.entries
            );
        }
        let mut regressed = false;
        // Coarse smoke alarm: relative p50 gate at +50%; the fine-grained
        // per-change gate is `just bench-micro-check` (ADR-0013).
        let gate = |new: u64, old: u64, floor: u64| new > (old.max(floor) * 3) / 2;
        for qb in &report.queries {
            let Some(prev) = old.queries.iter().find(|p| p.query == qb.query) else {
                continue;
            };
            if gate(qb.p50_us, prev.p50_us, 200) {
                anstream::eprintln!(
                    "{} {:<24} p50 {}→{}µs",
                    term::paint(term::ERROR, "REGRESSION"),
                    qb.query,
                    prev.p50_us,
                    qb.p50_us
                );
                regressed = true;
            }
            if qb.p99_us > P99_BUDGET_US {
                anstream::eprintln!(
                    "{} {:<24} p99 {}µs > {}µs acceptance line",
                    term::paint(term::ERROR, "OVER BUDGET"),
                    qb.query,
                    qb.p99_us,
                    P99_BUDGET_US
                );
                regressed = true;
            }
        }
        if let Some(new) = &report.restore
            && new.p50_ms > RESTORE_BUDGET_MS
        {
            anstream::eprintln!(
                "{} snapshot restore p50 {}ms > {}ms acceptance line",
                term::paint(term::ERROR, "OVER BUDGET"),
                new.p50_ms,
                RESTORE_BUDGET_MS
            );
            regressed = true;
        }
        if regressed {
            return Err("benchmark regression vs baseline".into());
        }
        anstream::eprintln!(
            "{}",
            term::paint(term::OK, &format!("no regression vs {}", path.display()))
        );
    }
    Ok(())
}
