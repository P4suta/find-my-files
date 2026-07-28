# ADR-0005: FRN index is a sorted id permutation

Date: 2026-06-11 / Status: Accepted

## Decision

The FRN→EntryId index is held only as a sorted id permutation (ids u32 = 4B/entry, index/frn.rs). The comparison key is the low-48-bit record number read by indirection into the full-FRN column. A key lookup returns the complete equal-key range across the unmerged tail and sorted body, always through the tombstone liveness filter.

`EntryId` identifies one directory link (one searchable path); the full 64-bit FRN identifies the underlying NTFS object generation. A hard-linked file therefore has multiple live EntryIds sharing one FRN. Link identity is `(full FRN, parent directory EntryId, original WTF-8 name)`. At most one full-FRN generation may be live for a low-48-bit record number, and directories retain exactly one row.

## Rationale

- An FxHashMap implementation is ~25B/entry (16-byte slot + bucket capacity padding + control bytes; real C: frn row 31.2MB), the largest RAM term after the name pool
- Splitting into two arrays keys u64 + ids u32 gives 12B/entry (frn row 31.2→15.1MB, WS 157→140B/entry; first time under the M0 gate ≤150B)
- keys is a pure redundant copy of masked(frn[ids[i]]) → removed to reach 4B/entry (−8B/entry, ~10MB on real C:)
- lookup is on the critical path only for the USN apply path and the builder's parent resolution; the search hot path does not touch it. The +1 cache miss from indirection is acceptable
- keeping duplicate FRNs in the existing id permutation adds no new per-row column and needs no snapshot or wire-format change
- Side benefit: restore goes from a million serial hashmap inserts → one parallel sort, criterion load_1m 89.4→58.9ms (−34%)

## Consequences

- Deletion is tombstone-only with no unmap. A final object delete or record reuse tombstones every live row in the FRN range; removal/rename of one hard link targets its exact link identity. Size/mtime and object attributes update every live row sharing the full FRN.
- `USN_REASON_HARD_LINK_CHANGE` is reconciled against the complete current link set; choosing one representative name or ignoring the reason is not an accepted degradation. USN reason flags accumulate, so a batch carrying both `FILE_DELETE` and `HARD_LINK_CHANGE` uses a three-state live read: a complete non-empty set wins and is reconciled, an exact proven-gone result removes all rows, and an incomplete/I/O-failed read rejects the whole batch before mutation, leaves its checkpoint unpublished, and forces a full rescan. The same preflight rule covers an ordinary rename whose exact old identity is missing while multiple links are live.
- Initial build coalesces duplicate `(full FRN, resolved parent, original name)` identities before publication. Snapshot restore rejects both duplicate live identities and any live row whose parent is not a live directory.
- Snapshot magic advances to **FMFIDX08** without changing the column layout: FMFIDX07 can be structurally valid yet semantically incomplete because it may persist only one representative name for a hard-linked object, so it must trigger a fresh MFT scan.
- The first-scan builder defers parent resolution and resolves it in bulk on the parallel path of finish() (per-lookup into the unmerged 1M tail is O(n²)). build_ms 13→64ms, invisible within the read-bound 2.1s scan

## Re-examination triggers

- If a design change lands where the search hot path requires an FRN lookup
- If Windows ever permits directory hard links through a supported API (the one-row directory invariant would need replacement)
