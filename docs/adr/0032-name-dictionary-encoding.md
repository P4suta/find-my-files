# ADR-0032: name dictionary-encoding (deduplicate the folded name pool)

Date: 2026-06-26 / Status: Accepted. The separate `dict_len` column described
below was dropped (lengths derive from the gapless `dict_off`, FMFIDX06 →
FMFIDX07) in [ADR-0033](0033-phase3-memory-latency-levers.md).

## Decision

Store each *distinct* folded name once in a `dict_pool`, and give each entry a
`name_id: u32` into a per-name `(dict_off: u32, dict_len: u16)` directory. The
per-entry `lower_pool` / `name_off` / `name_len` columns are removed.
`orig_pool` / `orig_off` stay per-entry (a shared folded name can back
differing originals — `README`/`readme` — so originals cannot dedup; ADR-0004).

## Measurement that justified it (`fmf stats C: --dict-estimate`, 1.76M entries)

- 52.2% of names are duplicates (D = 840,787 distinct of 1,760,684).
- Folded pool 49.5 MB → dict 32.8 MB: **−34% swept bytes** (cold-query speedup)
  and **net −8.7 B/entry** at rest. Below the −12 memory gate, but accepted for
  the *combined* memory + cold-scan-latency win (user decision, 2026-06-26).

## Key design points

- **`dict_off` is inherently sorted.** `name_id` is assigned in dict-append
  order, so `dict_off[0] < dict_off[1] < …` by construction. The sweep maps a
  hit offset → `name_id` with a monotonic cursor over `dict_off` directly —
  **the `OffsetTable` derived cache (build/extend/stale-pair logic in
  `query/memo.rs`) is deleted**, not ported. Stale gaps dissolve: a renamed
  entry points at a *new* `name_id`; the old name's bytes are addressed by
  `name_id`, never per-entry, so they are never "stale".
- **Sweep → `name_id` bitset.** `driver_candidates` sweeps `dict_pool` and sets
  a bit per matching `name_id` (boundary/anchor checks per dict name, as today).
  Materialization walks `perm_name` and keeps an entry when
  `name_id[id] ∈ bitset` (an O(1) bit test fused into the existing perm walk)
  **and** live/excluded/residuals pass — so no reverse `name_id → [entry]`
  index is needed. `refine` is untouched (it reads `name()`/`lower_name()`
  through the accessors, now dict-backed).
- **Unified append-then-dedup; transient interner.** Every push (initial scan
  *and* USN) appends a fresh `name_id` (no dedup on the hot path, no resident
  interner). `dedup_dict()` rebuilds `dict_pool`/`dict_off`/`dict_len` from the
  distinct live folded names with a *transient* `FxHashMap` interner and remaps
  every `name_id`; it runs at `finish()` and inside `compact()`. (A resident
  interner was rejected — it adds ~9 B/entry and erases the win.)
- **Churn trigger.** USN creates append un-deduped dict entries, so a
  pure-create burst (no tombstones) would bloat the dict without hitting the
  tombstone-driven compaction. A `dict_appends_since_dedup` counter triggers a
  `compact()` (which dedups) once it exceeds `live_len / 4`, bounding the bloat.
- **`dead_name_bytes` / `owned_name_bytes` change meaning.** A folded name's
  bytes are dead only when its last referrer goes; with dedup they are
  recomputed at `dedup_dict()` rather than charged per-tombstone. The
  `dead_name_bytes_tracks_pool_garbage` test is updated to the new semantics
  (a deliberate change, not a regression).

## Snapshot

FMFIDX05 → **FMFIDX06**. Sections gain `dict_pool` / `dict_off` / `dict_len` /
`name_id` and drop `lower_pool` / `name_off` / `name_len`. On-load validation:
`name_id[i] < D`, `dict_off[k] + dict_len[k] ≤ dict_pool.len()`, orig bounds use
`dict_len[name_id[i]]`. An FMFIDX05 file fails the magic → full rescan
(ADR-0010).

## Consequences

- `apply_batch` regresses (~2 → 3–5 ms): the `perm_name` merge comparator reads
  the folded name through the `name_id → dict_off` indirection (one extra cache
  miss, the same shape `index/frn.rs` already accepts). Gated at ≤ +25%.
- The fortified oracles must stay green: `refine == fresh` (exec proptest),
  `pool_scan`/`regex` naive oracles, the snapshot round-trips. The `sweep.rs`
  stale-gap tests are rewritten for dict semantics.

## Re-examination triggers

- If the cold-scan win fails to materialize (sweep no faster) or the
  `apply_batch` regression exceeds +25% on the real-volume gate, revert to the
  per-entry pool (ADR-0031 mtime saving is independent and stays).
