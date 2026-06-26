# ADR-0010: snapshot is a raw POD dump + full validation, no backward compatibility

Date: 2026-06-11 / Status: Accepted. The format has since evolved, each bump failing the magic check on the prior version → full rescan: **FMFIDX05** (mtime → u32 Unix-seconds, [ADR-0031](0031-mtime-u32-unix-seconds.md)); **FMFIDX06** (name dictionary-encoding — the `lower_pool`/`name_off`/`name_len` sections become `dict_pool`/`dict_off`/`dict_len`/`name_id`, [ADR-0032](0032-name-dictionary-encoding.md)); **FMFIDX07** (gapless dictionary — the `dict_len` section is dropped, each name's length derived from the next `dict_off`, [ADR-0033](0033-phase3-memory-latency-levers.md))

## Decision

Persistence is a homegrown binary **FMFIDX04**: magic + UsnJournalID + last USN + raw column-array dumps + xxhash64. Sections are lower_pool / orig_pool / orig_off / name_off / name_len / parent / size_lo / size-overflow ids+sizes / mtime / frn / flag / perm_name. No backward compatibility — a version mismatch or validation failure is always Err → full rescan.

## Rationale

- Real C:: 92.4MiB for 1.27M entries (−28% from the old 128.6MiB format), restore p50 81ms — ample margin against the restore→ready ≤2s gate
- A rescan is cheap at 2.0s (ADR-0011). Not worth the maintenance and test cost of migration code
- On load, beyond the checksum, perform structural validation of all slice bounds and overflow correspondence (Err → rescan instead of panicking on corrupt input)
- The size/mtime permutations and the FRN index are not persisted (parallel-sort rebuild at restore/first-use time is faster than a serial load: load_1m −34%, ADR-0005/0006)

## Impact

- Accept one full rescan per volume (2s-scale, requires elevation) on each format version bump
- structural_generation is not persisted (0 at restore). Since result handles do not cross processes, in-process monotonicity is sufficient
- Writes are temp → `MoveFileEx(REPLACE_EXISTING)`. Failures go to the snapshot_load_failures / snapshot_save_failures counters

## Re-examination trigger

- If a scale where the initial scan takes minutes becomes a primary target and the felt cost of a rescan per version bump becomes a problem
