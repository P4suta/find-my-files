//! JSON-shape golden pins in `contract/golden/` (repo root) — the engine's
//! serde output is contract surface the C# side deserializes, so its key
//! names and value forms are captured and pinned just like wire bytes
//! (ADR-0018). Re-capture only via `FMF_BLESS=1` (intentional change).
//!
//! Also pins `invalid_queries.json`: the shared fixture of query strings
//! the real parser/compiler rejects. The C# FakeEngineClient will use the
//! same file for its syntax verdicts (S5a) — pinning here keeps the fake's
//! idea of "invalid" from drifting away from the real engine's.

use std::path::PathBuf;

use fmf_core::diag::{ErrorEvent, Severity};
use fmf_core::metrics::{
    CountersSnapshot, Histogram, IndexStats, MetricsSnapshot, QueryTrace, ScanTrace, UsnTrace,
};
use fmf_core::query::{CaseMode, UtcResolver, compile, parse};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden")
}

fn bless_mode() -> bool {
    std::env::var("FMF_BLESS").as_deref() == Ok("1")
}

fn check_file(file: &str, bytes: &[u8]) {
    let path = golden_dir().join(file);
    if bless_mode() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        return;
    }
    let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{file}: cannot read golden file ({e}). Run the bless ritual \
             (FMF_BLESS=1) only for an intentional contract change."
        )
    });
    assert_eq!(
        on_disk, bytes,
        "{file}: golden JSON drifted. If intentional: docs/ARCHITECTURE.md \
         first, then FMF_BLESS=1 (ADR-0018)."
    );
}

/// True when the real engine rejects the query text as malformed.
fn is_rejected(q: &str) -> bool {
    match parse(q) {
        Err(_) => true,
        Ok(ast) => compile(&ast, CaseMode::Smart, &UtcResolver).is_err(),
    }
}

#[test]
fn invalid_queries_are_rejected_by_the_real_parser() {
    let candidates: &[&str] = &[
        "\"unterminated",
        "regex:[",
        "regex:(",
        "size:abc",
        "size:>",
        "size:1..x",
        "dm:notadate",
        "dm:2026-13-45",
        // NOTE: "ext:" (empty filter value) is *accepted* by the real parser
        // — the bless guard rejected it from this list. Do not re-add
        // candidates without verifying rejection.
    ];
    if bless_mode() {
        // The capture must only enshrine queries the engine actually
        // rejects — a candidate that parses fine is a bug in this list.
        let accepted: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|q| !is_rejected(q))
            .collect();
        assert!(
            accepted.is_empty(),
            "bless refused: these candidates are NOT rejected by the real \
             parser/compiler and must be removed from the fixture: {accepted:?}"
        );
        let doc = serde_json::json!({
            "comment": "Query strings the real engine rejects (parse or compile \
                        error → FMF_E_QUERY_SYNTAX). Shared fixture: fmf-core pins \
                        rejection here; the C# fake engine uses the same file for \
                        its syntax verdicts. Re-capture via FMF_BLESS=1.",
            "queries": candidates,
        });
        check_file(
            "invalid_queries.json",
            &serde_json::to_vec_pretty(&doc).unwrap(),
        );
        return;
    }
    let bytes = std::fs::read(golden_dir().join("invalid_queries.json"))
        .expect("invalid_queries.json missing — bless the corpus first");
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let queries = doc["queries"].as_array().expect("queries array");
    assert!(!queries.is_empty());
    for q in queries {
        let q = q.as_str().unwrap();
        assert!(
            is_rejected(q),
            "{q:?} is in invalid_queries.json but the engine now accepts it — \
             the shared fixture and the fake engine's verdicts have drifted"
        );
    }
}

/// One fully-populated QueryTrace — every field set to a distinct value so
/// a dropped or renamed serde key cannot hide.
fn sample_trace() -> QueryTrace {
    QueryTrace {
        query: "win ext:txt".into(),
        driver: "pool-scan".into(),
        cache: "refine".into(),
        unchanged: false,
        parse_us: 11,
        compile_us: 12,
        memo_us: 13,
        scan_us: 14,
        materialize_us: 15,
        merge_us: 16,
        total_us: 81,
        entries_scanned: 1_268_560,
        excluded_skipped: 17,
        hits: 18,
        volumes: 2,
    }
}

#[test]
fn query_trace_json_shape_is_pinned() {
    let bytes = serde_json::to_vec_pretty(&sample_trace()).unwrap();
    check_file("query_trace.json", &bytes);
}

#[test]
fn metrics_snapshot_json_shape_is_pinned() {
    let mut histogram = Histogram::new();
    for us in [1u64, 2, 3, 100, 1000, 10_000] {
        histogram.record(us);
    }
    let snapshot = MetricsSnapshot {
        recent_queries: vec![sample_trace()],
        p50_us: histogram.percentile_us(0.50),
        p99_us: histogram.percentile_us(0.99),
        query_histogram: histogram,
        recent_usn: vec![UsnTrace {
            volume: "C:".into(),
            records: 21,
            upserted: 22,
            deleted: 23,
            stat_updated: 24,
            stat_failures: 25,
            apply_us: 26,
        }],
        scans: vec![ScanTrace {
            volume: "C:".into(),
            source: "snapshot".into(),
            read_bytes: 31,
            read_ms: 32,
            mb_per_s: 33.5,
            parse_ms: 34,
            deferred_ms: 35,
            build_ms: 36,
            sort_ms: 37,
            total_ms: 38,
            entries: 39,
            peak_ws_bytes: 40,
        }],
        indexes: vec![IndexStats {
            volume: "C:".into(),
            entries: 41,
            live_entries: 42,
            tombstones: 43,
            name_pool_bytes: 44,
            lower_pool_bytes: 45,
            offsets_bytes: 46,
            parent_bytes: 47,
            size_bytes: 48,
            mtime_bytes: 49,
            frn_bytes: 50,
            flag_bytes: 51,
            permutations_bytes: 52,
            frn_map_bytes: 53,
            dead_name_bytes: 54,
            pool_garbage_ratio: 0.5,
            derived_cache_bytes: 55,
            total_bytes: 56,
            bytes_per_entry: 57.5,
            content_generation: 58,
            structural_generation: 59,
        }],
        counters: CountersSnapshot {
            stat_fetch_failures: 61,
            usn_batches_truncated: 62,
            snapshot_load_failures: 63,
            snapshot_save_failures: 64,
            deferred_names_unresolved: 65,
            corrupt_mft_records: 66,
            journal_rescans: 67,
            scan_pipeline_fallbacks: 68,
            offset_table_rebuild_fallbacks: 69,
            lazy_perm_rebuild_fallbacks: 70,
            compaction_aborts: 71,
            pipe_malformed_frames: 72,
            pipe_events_dropped: 73,
            pipe_connections_rejected: 74,
        },
        recent_errors: vec![ErrorEvent {
            seq: 81,
            uptime_ms: 82,
            severity: Severity::Warn,
            area: "fmf_core::scan".into(),
            volume: Some("C:".into()),
            message: "read-ahead spawn failed; fell back to sequential reads".into(),
        }],
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).unwrap();
    check_file("stats_snapshot.json", &bytes);
}
