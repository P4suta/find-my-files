# ADR-0033: Phase 3 memory/latency levers — gapless dictionary (FMFIDX07), predicate reorder, build-rank

Date: 2026-06-26 / Status: Accepted (amends ADR-0032)

## Context

Phase 1 (mtime u32, ADR-0031) and Phase 2 (name dictionary-encoding, ADR-0032)
banked the large wins: real-C: working set 95 → 77 B/entry (−18 B, ~19%) and
−34% swept bytes on a cold query. Phase 3 sweeps the *secondary* levers a
two-agent triage surfaced. Each is independent and small (−0.5…−1 B/entry, or
a cold-query polish); the value of this ADR is the record of which were taken,
which are gated on a measurement, and which are rejected *with numbers* so they
are not re-proposed (the ADR-0014 precedent).

## Decision (taken now)

Six levers land with Phase 3. All are byte-result-invariant and verified by
the fortified oracles (`refine == fresh`, the `pool_scan`/`regex` naive
oracles, the snapshot round-trips):

- **Predicate reorder (3a).** The materialize walk tests `name_id ∈ some
  group's sweep set` *before* the liveness/exclusion `flag` gather. AND is
  commutative, so the result is unchanged; on a selective query (`win`,
  `report`, `ext:dll`) ~90% of entries are rejected on the O(1) bit test before
  they ever touch `flag`. (`query/exec.rs`.)

- **Gapless dictionary — drop the `dict_len` column (Lever 2).** ADR-0032 gave
  each distinct name a `(dict_off: u32, dict_len: u16)` directory. Because names
  append contiguously and `name_id` is assigned in append order, `dict_off` is
  ascending and the pool is gapless, so a name's length is the gap to the next
  offset (`dict_pool.len()` for the last). The `dict_len` column is removed;
  `dict_off` becomes a `D`-entry CSR read as `dict_off[k+1] − dict_off[k]`.
  **−0.95 B/entry** beyond ADR-0032, and the `pool_end` branch in the
  sweep/compact loops dissolves. The snapshot bumps **FMFIDX06 → FMFIDX07**;
  on-load validation becomes "`dict_off` non-decreasing and within the pool"
  with each name's length derived from the next offset. This **supersedes the
  `dict_len` parts of ADR-0032**.

- **Interner pre-size (Lever 6).** `dedup_dict`'s transient `FxHashMap` interner
  is `with_capacity_and_hasher(n/2, …)` (live-distinct ≈ 48% on real C:),
  skipping the rehash growth across the ~1.76M inserts. Trivial,
  scan-throughput only.

- **Build-rank (1A).** The initial-build name sort ranks the `D` distinct
  dictionary names once by bytes, then sorts entries on a packed
  `(rank << 32) | id` u64 key. Distinct names → distinct ranks, so it is
  **byte-identical** to a full `cmp_by(Name)` sort (a test pins the equality)
  while replacing a dictionary deref per comparison with a single integer
  compare. Build-time only (`index/builder.rs`); `compact()` still remaps
  without sorting and the USN merge keeps its `cmp_by` insertion. The sort drops
  ~286 → ~170 ms — invisible to the user (a 2.3 s scan inside a 60 s budget),
  taken for completeness.

- **Original-spelling dedup (Lever 1, table-free).** The originals that differ
  from their fold (`README`, `LICENSE`, every capitalized name) duplicate
  heavily — real C:: 562k differing entries fold to 221k distinct originals.
  `dedup_orig` interns them and points each `orig_off` at the one shared copy.
  **No offset table and no format bump**: the fold is length-preserving
  (ADR-0004), so an original's length is its entry's folded length
  (`name_len_of`), and the `orig_pool`/`orig_off` snapshot sections keep their
  shape (just a smaller, deduped pool). **−4.5 B/entry** at rest — the
  `--dict-estimate` gate measured −3.9 against an *assumed* gapless
  `orig_dict_off` table (`+4·D_orig`); deriving the length from the fold drops
  that table and beats the projection. Runs beside `dedup_dict` at
  `finish`/`compacted` (`index/core.rs`).

- **apply_batch decoration.** The USN-batch merge sorts the new entries by name
  before splicing them into `perm_name`; ADR-0032's dict indirection made each
  comparison resolve two folded names through `name_id → dict_off`. The sort now
  decorates each batch entry with its resolved name once and sorts on the
  borrowed slices — byte-identical order (name then id), the dict deref paid
  O(B) times instead of O(B·log B) (`index/mutate.rs`).

## Triage (figures from `fmf stats C: --dict-estimate`, 1.76M entries)

| Lever | Effect | Path | Verdict |
|---|---|---|---|
| 3a predicate reorder | ~90% flag gathers skipped on selective queries | cold query | **GO** |
| 2 gapless dict (drop `dict_len`) | −0.95 B/entry | memory | **GO (FMFIDX07)** |
| 6 interner pre-size | dedup ~10-20% | scan | **GO** |
| 1A build-rank | sort 286 → 170 ms (invisible) | scan throughput | **GO** |
| 1 orig-pool dedup (table-free) | **−4.5 B/entry** (real C:: 562k differing → 221k distinct) | memory | **GO (realized)**: `--dict-estimate` orig net −3.9 ≤ −1.5; length from the fold drops the offset table → −4.5, no format bump |
| 3b `_mm_prefetch` software prefetch | no measurable win | cold query | **REJECTED (tried, removed)**: the one real-volume A/B was thermally confounded (the prefetch-on run's indexing — which prefetch cannot touch — ran +45%), and a perm-order gather is already served by the hardware prefetcher; not worth a branch in the hottest loop |
| apply_batch decoration | resolves each batch name once, not per comparison | apply_batch | **GO**: byte-identical merge order, O(B) dict derefs vs O(B·log B) |
| frn 48-bit packing | −2.0 B/entry | memory | **NO-GO**: the FFI contract freezes `FmfRow.frn: u64` |
| full-scan numeric SIMD | — | cold query | **NO-GO**: `FullScan` gathers in perm order; the residual is not a contiguous scan |
| `#[target_feature]` multi-versioning | — | — | **NO-GO**: no hot arithmetic loop to vectorize (the sweep is `memchr`, the walk is a gather) |
| resident name-rank column | — | apply_batch | **NO-GO**: +1.9 B/entry, and the merge compares freshly-appended (un-ranked) `name_id`s, so a resident rank cannot serve it |
| `perm_name` / `frn_map` derivation | — | — | **NO-GO**: both are load-bearing (the merge target and the FRN lookup), not caches |
| flag / parent bit-packing | — | hot read | **NO-GO**: POD snapshot columns, a 4 GiB parent ceiling, and a per-read mask on the hottest gather |
| 8 sweep bitset shards | dense-needle alloc | cold query micro | **REJECT**: per-thread bitset shards regress the common *sparse* needle (alloc + clear a `D/8` bitset to set a handful of bits); the current small `Vec<Vec<u32>>` wins |

## Consequences

- The snapshot is FMFIDX07; an FMFIDX06 (or older) file fails the magic check →
  full rescan (ADR-0010, no compat). The `valid_sections` fixture and the
  structural validator drop `dict_len` and derive each name's length from the
  gapless `dict_off`.
- `compute_dict_estimate`'s historical projection still prints the Phase-2
  figure with the `+6·D` directory cost (`dict_off` + `dict_len`); Lever 2
  realized `+4·D`, an extra −2 B/entry. The Lever-1 estimator
  (`compute_orig_estimate`) prints the realized *table-free* net (no offset
  table — the original's length comes from the fold), so it reports ≈−4.5
  where the original projection assumed a `+4·D_orig` table and read −3.9.
- No contract/golden re-bless: `IndexStats`, the counters, and the CLI surface
  are unchanged (the `IndexStats` pool/offset fields are reused, ADR-0032).

## Re-examination triggers

- A real need for sub-second mtime ordering reverts ADR-0031, not this ADR.
- If the real-volume gate shows `apply_batch` over +25% or the cold-query p99
  regressing, revert the offending lever — each is independent (the gapless
  dict, 3a, and build-rank do not depend on one another).
- 3b software prefetch was implemented, measured, and removed (its only A/B was
  thermally confounded). Re-propose it only with a clean cold-machine A/B that
  shows a real materialize win over the hardware prefetcher.
