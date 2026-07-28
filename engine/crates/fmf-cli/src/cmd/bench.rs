//! `bench` — the fixed real-volume benchmark set + the baseline regression
//! gate (discipline: ADR-0013).

use std::time::Instant;

use fmf_core::query::QueryOptions;
use serde::Serialize;

use super::ctx::Ctx;
use super::{build_index, run_query, term};
use crate::bench_support::{
    BENCH_QUERIES, BenchReport, QueryBench, bench_restore, entry_count_within_baseline,
    index_budget_ms, median, working_set_within_budget,
};

const P99_BUDGET_US: u64 = 50_000;
const RESTORE_BUDGET_MS: u64 = 1_000;
const P50_BASELINE_FLOOR_US: u64 = 200;

#[derive(Debug, Serialize)]
struct RealCase {
    id: String,
    subject: String,
    actual: f64,
    baseline: f64,
    delta: f64,
    delta_ratio: f64,
    threshold_metric: &'static str,
    threshold_value: f64,
    unit: &'static str,
    verdict: &'static str,
}

#[derive(Debug, Serialize)]
struct RealEvidence {
    schema: u32,
    kind: &'static str,
    expected_cases: Vec<String>,
    cases: Vec<RealCase>,
    finite: bool,
    passed: bool,
    errors: Vec<String>,
}

struct RealCaseInput {
    id: String,
    subject: String,
    actual: f64,
    baseline: f64,
    threshold_metric: &'static str,
    threshold_value: f64,
    unit: &'static str,
    passes: bool,
}

fn expected_case_names() -> Vec<String> {
    let mut names = vec![
        "corpus/entries".to_owned(),
        "index/initial".to_owned(),
        "memory/ready".to_owned(),
    ];
    for index in 0..BENCH_QUERIES.len() {
        names.push(format!("query/{index:02}/p50"));
        names.push(format!("query/{index:02}/p99"));
    }
    names.push("snapshot/restore-p50".to_owned());
    names
}

fn ratio_delta(actual: f64, baseline: f64) -> Result<f64, String> {
    if !actual.is_finite() || !baseline.is_finite() || baseline <= 0.0 {
        return Err(format!(
            "cannot compute a finite ratio from actual={actual}, baseline={baseline}"
        ));
    }
    Ok((actual - baseline) / baseline)
}

fn case(input: RealCaseInput) -> Result<RealCase, String> {
    let delta = input.actual - input.baseline;
    let delta_ratio = ratio_delta(input.actual, input.baseline)?;
    if !delta.is_finite() || !input.threshold_value.is_finite() {
        return Err(format!(
            "performance case `{}` contains a non-finite value",
            input.id
        ));
    }
    Ok(RealCase {
        id: input.id,
        subject: input.subject,
        actual: input.actual,
        baseline: input.baseline,
        delta,
        delta_ratio,
        threshold_metric: input.threshold_metric,
        threshold_value: input.threshold_value,
        unit: input.unit,
        verdict: if input.passes { "pass" } else { "fail" },
    })
}

fn evaluate(report: &BenchReport, old: &BenchReport) -> Result<RealEvidence, String> {
    if report.entries == 0 || old.entries == 0 {
        return Err(
            "real-volume evidence requires non-empty actual and baseline corpora".to_owned(),
        );
    }
    let actual_queries: Vec<_> = report
        .queries
        .iter()
        .map(|query| query.query.as_str())
        .collect();
    let baseline_queries: Vec<_> = old
        .queries
        .iter()
        .map(|query| query.query.as_str())
        .collect();
    if actual_queries != BENCH_QUERIES || baseline_queries != BENCH_QUERIES {
        return Err(format!(
            "real-volume query contract mismatch; expected={BENCH_QUERIES:?}; \
             actual={actual_queries:?}; baseline={baseline_queries:?}"
        ));
    }
    let actual_restore = report
        .restore
        .as_ref()
        .ok_or_else(|| "actual benchmark report has no restore measurement".to_owned())?;
    let baseline_restore = old
        .restore
        .as_ref()
        .ok_or_else(|| "baseline benchmark report has no restore measurement".to_owned())?;
    if report.index_ms == 0
        || old.index_ms == 0
        || report.working_set_bytes == 0
        || old.working_set_bytes == 0
        || actual_restore.p50_ms == 0
        || baseline_restore.p50_ms == 0
    {
        return Err("real-volume report contains a zero required measurement".to_owned());
    }

    let mut cases = Vec::with_capacity(expected_case_names().len());
    let entries_actual = report.entries as f64;
    let entries_baseline = old.entries as f64;
    cases.push(case(RealCaseInput {
        id: "corpus/entries".to_owned(),
        subject: report.volume.clone(),
        actual: entries_actual,
        baseline: entries_baseline,
        threshold_metric: "absolute_delta_ratio",
        threshold_value: 0.10,
        unit: "entries",
        passes: entry_count_within_baseline(report.entries, old.entries),
    })?);
    cases.push(case(RealCaseInput {
        id: "index/initial".to_owned(),
        subject: report.volume.clone(),
        actual: report.index_ms as f64,
        baseline: old.index_ms as f64,
        threshold_metric: "actual",
        threshold_value: index_budget_ms(report.entries) as f64,
        unit: "milliseconds",
        passes: report.index_ms <= index_budget_ms(report.entries),
    })?);
    let actual_bytes_per_entry = report.working_set_bytes as f64 / entries_actual;
    let baseline_bytes_per_entry = old.working_set_bytes as f64 / entries_baseline;
    cases.push(case(RealCaseInput {
        id: "memory/ready".to_owned(),
        subject: report.volume.clone(),
        actual: actual_bytes_per_entry,
        baseline: baseline_bytes_per_entry,
        threshold_metric: "actual",
        threshold_value: 110.0,
        unit: "bytes_per_entry",
        passes: working_set_within_budget(report),
    })?);

    for (index, (actual, baseline)) in report.queries.iter().zip(&old.queries).enumerate() {
        let p50_threshold = baseline.p50_us.max(P50_BASELINE_FLOOR_US) as f64 * 1.5;
        cases.push(case(RealCaseInput {
            id: format!("query/{index:02}/p50"),
            subject: actual.query.clone(),
            actual: actual.p50_us as f64,
            baseline: baseline.p50_us as f64,
            threshold_metric: "actual",
            threshold_value: p50_threshold,
            unit: "microseconds",
            passes: actual.p50_us as f64 <= p50_threshold,
        })?);
        cases.push(case(RealCaseInput {
            id: format!("query/{index:02}/p99"),
            subject: actual.query.clone(),
            actual: actual.p99_us as f64,
            baseline: baseline.p99_us as f64,
            threshold_metric: "actual",
            threshold_value: P99_BUDGET_US as f64,
            unit: "microseconds",
            passes: actual.p99_us <= P99_BUDGET_US,
        })?);
    }
    cases.push(case(RealCaseInput {
        id: "snapshot/restore-p50".to_owned(),
        subject: report.volume.clone(),
        actual: actual_restore.p50_ms as f64,
        baseline: baseline_restore.p50_ms as f64,
        threshold_metric: "actual",
        threshold_value: RESTORE_BUDGET_MS as f64,
        unit: "milliseconds",
        passes: actual_restore.p50_ms <= RESTORE_BUDGET_MS,
    })?);

    let finite = cases.iter().all(|case| {
        [
            case.actual,
            case.baseline,
            case.delta,
            case.delta_ratio,
            case.threshold_value,
        ]
        .into_iter()
        .all(f64::is_finite)
    });
    let passed = finite && cases.iter().all(|case| case.verdict == "pass");
    Ok(RealEvidence {
        schema: 1,
        kind: "real-verdict",
        expected_cases: expected_case_names(),
        cases,
        finite,
        passed,
        errors: Vec::new(),
    })
}

fn failure_evidence(error: &str) -> RealEvidence {
    RealEvidence {
        schema: 1,
        kind: "real-verdict",
        expected_cases: expected_case_names(),
        cases: Vec::new(),
        finite: false,
        passed: false,
        errors: vec![error.to_owned()],
    }
}

fn write_evidence(
    path: &std::path::Path,
    evidence: &RealEvidence,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(evidence)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn bench(
    drive: &str,
    out: Option<&std::path::Path>,
    baseline: Option<&std::path::Path>,
    evidence_path: Option<&std::path::Path>,
    ctx: Ctx,
) -> Result<(), Box<dyn std::error::Error>> {
    let index_started = Instant::now();
    let idx = build_index(drive, ctx)?;
    let index_ms = index_started.elapsed().as_millis() as u64;
    // Capture the Ready-state working set before user-demanded lazy caches are
    // built. This is the steady state shared by the CLI and the application.
    let working_set_bytes = fmf_core::mft::current_working_set();
    let opt = QueryOptions::default();

    let mut report = BenchReport {
        volume: drive.to_string(),
        entries: idx.len() as u64,
        index_ms,
        working_set_bytes,
        peak_working_set_bytes: 0,
        queries: Vec::new(),
        restore: None,
    };

    if !ctx.is_json() {
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
    }
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
        if !ctx.is_json() {
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
        }
        report.queries.push(qb);
    }
    report.peak_working_set_bytes = fmf_core::mft::peak_working_set();
    report.restore = Some(bench_restore(&idx)?);
    if ctx.is_json() {
        // The whole report goes to stdout as one document; the human table and
        // the baseline verdict below stay on text/stderr.
        super::json::emit(&report)?;
    } else {
        println!("initial index {} ms", report.index_ms);
        println!(
            "ready working set {:.1} MiB ({:.1} B/entry)",
            report.working_set_bytes as f64 / (1024.0 * 1024.0),
            if report.entries == 0 {
                0.0
            } else {
                report.working_set_bytes as f64 / report.entries as f64
            }
        );
        println!(
            "peak working set {:.1} MiB",
            report.peak_working_set_bytes as f64 / (1024.0 * 1024.0)
        );
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
    }

    if let Some(path) = out {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        eprintln!("report written to {}", path.display());
    }

    if let Some(path) = baseline {
        // Tail latency and restore are gated on *absolute* acceptance
        // budgets, never relative (ADR-0013).
        let old: BenchReport = match std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
        {
            Ok(old) => old,
            Err(error) => {
                let message = format!("failed to load baseline {}: {error}", path.display());
                if let Some(evidence_path) = evidence_path {
                    write_evidence(evidence_path, &failure_evidence(&message))?;
                }
                return Err(message.into());
            }
        };
        let decision = match evaluate(&report, &old) {
            Ok(decision) => decision,
            Err(error) => {
                let decision = failure_evidence(&error);
                if let Some(evidence_path) = evidence_path {
                    write_evidence(evidence_path, &decision)?;
                }
                return Err(error.into());
            }
        };
        if let Some(evidence_path) = evidence_path {
            write_evidence(evidence_path, &decision)?;
        }

        if decision
            .cases
            .iter()
            .find(|case| case.id == "corpus/entries")
            .is_some_and(|case| case.verdict == "fail")
        {
            anstream::eprintln!(
                "{} entries drifted {}→{} (>10%) since the baseline was recorded — \
                 refusing an unreliable verdict; run `just bench-baseline` deliberately",
                term::paint(term::ERROR, "STALE BASELINE"),
                old.entries,
                report.entries
            );
        }
        let index_budget = index_budget_ms(report.entries);
        if report.index_ms > index_budget {
            anstream::eprintln!(
                "{} initial index {}ms > {}ms acceptance line for {} entries",
                term::paint(term::ERROR, "OVER BUDGET"),
                report.index_ms,
                index_budget,
                report.entries
            );
        }
        if !working_set_within_budget(&report) {
            anstream::eprintln!(
                "{} ready working set {:.1} B/entry > 110 B/entry acceptance line",
                term::paint(term::ERROR, "OVER BUDGET"),
                report.working_set_bytes as f64 / report.entries as f64
            );
        }
        for (qb, prev) in report.queries.iter().zip(&old.queries) {
            let p50_threshold = prev.p50_us.max(P50_BASELINE_FLOOR_US) as f64 * 1.5;
            if qb.p50_us as f64 > p50_threshold {
                anstream::eprintln!(
                    "{} {:<24} p50 {}→{}µs",
                    term::paint(term::ERROR, "REGRESSION"),
                    qb.query,
                    prev.p50_us,
                    qb.p50_us
                );
            }
            if qb.p99_us > P99_BUDGET_US {
                anstream::eprintln!(
                    "{} {:<24} p99 {}µs > {}µs acceptance line",
                    term::paint(term::ERROR, "OVER BUDGET"),
                    qb.query,
                    qb.p99_us,
                    P99_BUDGET_US
                );
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
        }
        if !decision.passed {
            return Err("benchmark regression vs baseline".into());
        }
        anstream::eprintln!(
            "{}",
            term::paint(term::OK, &format!("no regression vs {}", path.display()))
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_support::{BenchReport, QueryBench, RestoreBench};

    fn report() -> BenchReport {
        BenchReport {
            volume: "C:".to_owned(),
            entries: 1_000_000,
            index_ms: 10_000,
            working_set_bytes: 100_000_000,
            peak_working_set_bytes: 120_000_000,
            queries: BENCH_QUERIES
                .iter()
                .map(|query| QueryBench {
                    query: (*query).to_owned(),
                    hits: 1,
                    p50_us: 1_000,
                    p99_us: 2_000,
                    max_us: 3_000,
                    p50_memo_us: 1,
                    p50_scan_us: 1,
                    p50_materialize_us: 1,
                    cold_us: 1_000,
                })
                .collect(),
            restore: Some(RestoreBench {
                file_bytes: 1_000,
                entries: 1_000_000,
                save_ms: 10,
                p50_ms: 100,
                min_ms: 90,
            }),
        }
    }

    #[test]
    fn real_evidence_is_complete_deterministic_and_fail_closed() {
        let baseline = report();
        let mut actual = report();
        let evidence = evaluate(&actual, &baseline).unwrap();
        assert_eq!(evidence.expected_cases, expected_case_names());
        assert_eq!(evidence.cases.len(), 24);
        assert!(evidence.finite);
        assert!(evidence.passed);

        actual.queries[0].p50_us = 1_501;
        let failed = evaluate(&actual, &baseline).unwrap();
        assert!(!failed.passed);
        let case = failed
            .cases
            .iter()
            .find(|case| case.id == "query/00/p50")
            .unwrap();
        assert!((case.threshold_value - 1_500.0).abs() < f64::EPSILON);
        assert_eq!(case.verdict, "fail");
    }

    #[test]
    fn real_evidence_rejects_query_contract_drift() {
        let baseline = report();
        let mut actual = report();
        actual.queries.swap(0, 1);
        assert!(evaluate(&actual, &baseline).is_err());
    }
}
