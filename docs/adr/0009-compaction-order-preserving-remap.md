# ADR-0009: compaction is old-id ascending remap (no re-sort)

Date: 2026-06-11 / Status: Accepted

## Decision

Compaction reclaims tombstone rows and dead bytes in the pool. Live entries are renumbered in old-id ascending order — because relative order is preserved, every (key, id)-ordered structure (perm_name, FRN index) is carried over by an O(n) filter+remap copy with no re-sort. The volume thread evaluates the threshold on each batch apply: `len≥100k AND (tombstone_ratio>12.5% OR dead_name_bytes>32MiB)`.

## Rationale

- Tombstone rows and the name bytes abandoned by renames accumulate without bound — a slow leak against the B/entry RAM gate (previously reclaimable only by a full rescan)
- With old-id ascending remap, the relative order of live entries is preserved, so sorted structures can be filtered+remapped byte-equivalently (O(n), zero sort cost)
- The decision input is dead_name_bytes observability (IndexStats.pool_garbage_ratio). Thresholds are set on the premise of real-volume observation

## Impact

- The copy build runs under a read guard (queries run concurrently; the only writer is the single volume thread). The swap goes through `install_index` with a µs-scale write lock + structural generation bump
- Result handles open across a compaction go hard STALE (`FMF_E_STALE`) → the UI auto-reissues the same query (existing mechanism)
- Children of dead directories are reparented to root (same as the orphan policy of push_raw)
- A defensive generation-check failure increments the `compaction_aborts` counter + discards the copy (detection of a broken single-writer invariant; does not stay silent)

## Re-examination trigger

- Observation of `compaction_aborts > 0` (revisit the single-writer invariant)
- If the thresholds show, in real operation, that compaction fires too often or reclaims too little (threshold re-tuning)
