# ADR-0005: FRN index is a sorted id permutation

Date: 2026-06-11 / Status: Accepted

## Decision

The FRN→EntryId index is held only as a sorted id permutation (ids u32 = 4B/entry, index/frn.rs). The comparison key is read by indirection into the frn column. lookup scans the unmerged tail newest-first (the latest upsert within a batch wins) → binary-searches the body, always passing through the tombstone liveness filter.

## Rationale

- An FxHashMap implementation is ~25B/entry (16-byte slot + bucket capacity padding + control bytes; real C: frn row 31.2MB), the largest RAM term after the name pool
- Splitting into two arrays keys u64 + ids u32 gives 12B/entry (frn row 31.2→15.1MB, WS 157→140B/entry; first time under the M0 gate ≤150B)
- keys is a pure redundant copy of masked(frn[ids[i]]) → removed to reach 4B/entry (−8B/entry, ~10MB on real C:)
- lookup is on the critical path only for the USN apply path and the builder's parent resolution; the search hot path does not touch it. The +1 cache miss from indirection is acceptable
- Side benefit: restore goes from a million serial hashmap inserts → one parallel sort, criterion load_1m 89.4→58.9ms (−34%)

## Consequences

- Deletion is tombstone-only with no unmap. rename / NTFS record reuse leaves dead duplicates of the same key, but under the liveness filter live count is always at most 1 (pinned by a byte-identical test against the forward-merge reference under random rename/delete storms)
- The first-scan builder defers parent resolution and resolves it in bulk on the parallel path of finish() (per-lookup into the unmerged 1M tail is O(n²)). build_ms 13→64ms, invisible within the read-bound 2.1s scan

## Re-examination triggers

- If a design change lands where the search hot path requires an FRN lookup
