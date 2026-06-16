# ADR-0007: size column is u32 + overflow map

Date: 2026-06-11 / Status: Accepted

## Decision

Hold the size column as u32. For 4GiB and above, store sentinel `u32::MAX` and offload the real value to an overflow map keyed by entry.

## Rationale

- Measured on real C:: 10 of 1,268,450 files exceed 4GiB (0.0008%)
- −4B/entry (u64→u32)
- The sentinel branch in `size()` is effectively zero-cost; the map is negligible in size

## Impact

- The snapshot (FMFIDX04) gains a size-overflow section (ids+sizes). On load, structurally validate "all pairs ↔ sentinel correspondence, ascending order, truly overflowing"
- Files that shrink below 4GiB are correctly returned to the u32 side via the sentinel

## Re-examination trigger

- If a volume where the share of files over 4GiB reaches the several-percent range (e.g. a dedicated video-archive machine) becomes a primary target
