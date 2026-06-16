# ADR-0004: fold-overflow name layout

Date: 2026-06-11 / Status: Accepted

## Decision

The only full-length pool that can be swept contiguously is the single folded `lower_pool`. The original text is stored in `orig_pool` only when it differs from the fold, referenced by `orig_off` (u32, sentinel `u32::MAX` = identical to fold). `name()` lends the lower slice directly for fold-identical entries.

## Rationale

- Real C: measurement (1,268,450 entries): fold-identical (lower == orig byte match) = 73.2%. About 3/4 of the double-stored names are duplicate identical bytes
- Measured −16B/entry. The single largest term toward the M2 RAM gate (≤110B/entry)
- Three soundness pillars: (1) the fold is length-preserving (ADR-0003) (2) original match ⇒ fold match (a superset sweep is sound; same algebra as bridge_needle in subsume.rs) (3) a fold-unstable needle (a needle differing from its own fold) cannot appear in a fold-identical name → an O(1) rejection via a single `orig_off` sentinel resolves 73% of candidates
- Alternative (i) "sorted (entry, orig_off) pairs" is predicted at −19.6B/entry, 1.9B better than the u32-column approach (−17.7B), but adds a binary search per residual verification, so the u32 column was chosen to avoid p99 risk

## Consequences

- Needles containing uppercase (smart case) and Sensitive mode cannot sweep the original directly; they become a superset sweep of the folded needle + original-text residual verification
- Accepted regression: real C: uppercase needle ("Win", smart-case) p50 2.5→3.6ms (7% of the p99 budget of 50ms)
- The snapshot shrinks by the same amount (FMFIDX04)

## Re-examination triggers

- If a name distribution is observed on a real volume where the fold-identical ratio collapses substantially (below 50%-class)
