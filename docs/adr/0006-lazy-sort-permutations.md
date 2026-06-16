# ADR-0006: Lazy sort permutations (only perm_name is always maintained)

Date: 2026-06-11 / Status: Accepted

## Decision

The only always-maintained, persisted fast-sort permutation is perm_name. size/mtime order is a derived-cache lazy permutation (SizePerm/MtimePerm in query/memo.rs): one par_sort on the first sort query, then per-generation incremental extension via the same insertion-position merge as perm_name; not included in the snapshot (non-persistent).

## Rationale

- −8B/entry (two permutations' worth) + ~8MB snapshot reduction. Many sessions never request a size/mtime sort
- Initial construction is one par_sort, ~60ms-class @1M. One-off, so it does not sit on the always-on path of the query p99 gate (50ms)
- Maintenance cost reduction: apply_batch_1k 6.67→1.96ms (−71.6%; permutations to merge go 3→1)
- The only regression from going lazy is first_query_sorted_size +6.5% (2.0→2.1ms, within the 10% gate)

## Consequences

- The first size/mtime column click accepts a one-off construction cost (~60ms-class @1M)
- After snapshot restore, the first use re-sorts (stale order from stat updates is also reset at the same time)
- Correctness is pinned by an extend oracle (byte-equality of lazy == fresh-sort). Watermark inconsistency triggers warn + `lazy_perm_rebuild_fallbacks` counter + full rebuild (does not go silent)

## Re-examination triggers

- Real demand for a single volume large enough that the measured first sort-click exceeds the perceptual threshold (100ms-class)
