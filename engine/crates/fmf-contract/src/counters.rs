//! Degradation-counter names — the snake_case keys of
//! `MetricsSnapshot.counters` in the stats JSON. This JSON shape is contract
//! surface (the C# CountersData is generated from this list, and fmf-core's
//! golden_json test pins its CountersSnapshot serde keys against it), so the
//! list lives here even though the counters themselves are engine-internal.
//! Append new counters at the end; never rename (F12 history and the golden
//! stats_snapshot.json key on them).

pub const COUNTER_NAMES: &[&str] = &[
    "stat_fetch_failures",
    "usn_batches_truncated",
    "snapshot_load_failures",
    "snapshot_save_failures",
    "deferred_names_unresolved",
    "corrupt_mft_records",
    "journal_rescans",
    "scan_pipeline_fallbacks",
    "offset_table_rebuild_fallbacks",
    "lazy_perm_rebuild_fallbacks",
    "compaction_aborts",
    "pipe_malformed_frames",
    "pipe_events_dropped",
    "pipe_connections_rejected",
];
