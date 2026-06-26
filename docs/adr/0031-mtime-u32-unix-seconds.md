# ADR-0031: mtime as a u32 Unix-seconds column

Date: 2026-06-26 / Status: Accepted

## Decision

The per-entry `mtime` column stores Unix-epoch **seconds** in a `u32`, not the
raw Windows FILETIME tick count (100 ns since 1601) in an `i64`. The full
FILETIME is reconstructed to the second on read (`VolumeIndex::mtime`), so the
FFI contract (`FmfRow.mtime: i64`) and every consumer stay byte-unchanged.
Encode/decode is the single pair `query::dates::mtime_{ticks_to_secs,
secs_to_ticks}`. `0` is a reserved "unknown timestamp" sentinel: a 0 tick (a
failed stat fetch) and every pre-1970 tick collapse to it and reconstruct back
to a 0 tick. The snapshot bumps to FMFIDX05 (amends ADR-0010).

## Rationale

- **−4 B/entry** (8 → 4). On real C: (~1.27M entries) ≈ 5 MiB, ~4–5% of the
  resident index — the single largest *clean* column saving. The name pools
  dominate but are already minimized (ADR-0003/0004); `size` is u32+overflow
  (ADR-0007); `frn` is the FrnIndex backing store; `parent`/`flag`/`perm_name`
  are fixed.
- **No observable behavior change on real data.** `dm:` bounds are day-aligned
  (`filetime_at_midnight`), so second-granularity storage yields byte-identical
  filter results. Sort order is preserved (the map is monotonic); only the
  sub-second tie-break between files modified in the same second is lost (it
  falls to the deterministic id tie-break — imperceptible for a filename
  search). Unix seconds cover 1970–2106; pre-1970/post-2106 saturate.
- The `0` sentinel keeps an "unknown timestamp" (failed stat, FILETIME 0)
  filtering and displaying exactly as before (1601), not snapping to 1970.

## Consequences

- Dates strictly between 1601 and 1970 are no longer representable (they
  collapse to the 0/unknown sentinel). No real NTFS file carries such an mtime;
  only synthetic fixtures did, now anchored past 1970.
- Snapshot is FMFIDX05; an FMFIDX04 file fails the magic check → full rescan
  (ADR-0010, no compat). The mtime section is a `u32` column.
- IndexStats `mtime_bytes` halves; the field and the wire contract are
  structurally unchanged (no contract/golden re-bless).

## Re-examination triggers

- A real need for sub-second mtime ordering, or for mtimes outside 1970–2106.
