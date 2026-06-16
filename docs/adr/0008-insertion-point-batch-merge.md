# ADR-0008: USN batch merge via insertion-point binary search + in-place segment move

Date: 2026-06-11 / Status: Accepted

## Decision

Applying a USN batch to sorted structures (perm_name, FRN index; index/mod.rs `merge_sorted_tail`) finds each batch element's insertion point by binary search and moves the intervening segment once each with `copy_within`. No full-length element comparison, no full-length reallocation. Capacity is reserved with `reserve_exact(max(add, len/64))`.

## Rationale

- Batch ~1k vs existing ~1M. The full-length rebuild approach paid, per batch, a comparison against every existing element (for perm_name, a string comparison for every file in the index)
- Measured: apply_batch_1k 54.6→2.0ms@1M (54.6→6.3ms from the insertion-point merge; 2.0ms with permutation lazification = ADR-0006 included). The ~30MB/batch reallocation churn on the FRN index side also disappears
- Complexity is O(batch·log n) comparisons + one bounded memmove, no allocation
- A doubling capacity policy puts a permanent 2× slack on the RAM gate. With a len/64 floor, the full-length copy is amortized over each ~1.6% growth, and the slack ceiling is also ~1.6%

## Impact

- Existing elements are not reordered. Because the id tie-break makes the sort result unique, byte-identity with the old code can be pinned in tests (random-batch comparison against a forward-merge reference)
- In-place stat updates leave perm_size/perm_mtime locally stale-sorted (as before; under the purview of lazy permutation = ADR-0006)

## Re-examination trigger

- If usage where the batch length is large relative to the existing length becomes the norm (in that regime, full-length merge wins)
