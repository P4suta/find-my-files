# ADR-0002: Linear pool sweep + incremental search (no trigram inverted index)

Date: 2026-06-11 / Status: Accepted

## Decision

Search is a linear sweep of the folded name pool (SIMD memmem, rayon 64k-chunk parallelism). A re-query that provably narrows the previous query is handled by `query::refine`, which re-evaluates only the previous hit set (conservative subsumption rules in query/subsume.rs). No trigram inverted index.

## Rationale

- A synthetic 1M-entry cold 3-char query is about 2.9ms (query-cache MISS + derived-cache warm, materialize included). That is an order of magnitude below the criterion "per-volume scan_us p99 > 25ms @1M"
- Posting maintenance costs +10-15B/file under the RAM ≤110B/file constraint, plus diff maintenance per USN batch. Not worth it
- Incremental search is O(previous hit count), skipping both the scan and the O(n) materialize

## Consequences

- refine applies only under conservative subsumption rules (same sort, single AND group, needle containment / range shrink / filter addition only). Correctness is held by an oracle property test (refine == fresh search)
- Kill switch `FMF_QUERY_CACHE=0`; observability via `QueryTrace.cache` (miss/refine/partial)

## Re-examination triggers (only if all hold)

1. Cache-MISS cold 3-char scan_us p99 > 25ms @1M
2. Measured estimate from `fmf stats --trigram-estimate` ≤15B/file and total ≤110B/file
3. Posting diff maintenance ≤2ms/batch
4. Real demand for a single volume exceeding 4M entries
