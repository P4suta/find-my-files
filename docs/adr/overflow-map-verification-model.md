---
date: 2026-11-06
---

# Overflow map verification model

## Status

Proposed.

## Context

HashMap internals and symbolic heap shapes exhaust the index proof budget.
Kani recommends [decomposition](https://github.com/model-checking/kani/issues/3965).

## Decision

Keep the production overflow map as `FxHashMap<EntryId, u64>`.
Model the required [HashMap contracts](https://doc.rust-lang.org/std/collections/struct.HashMap.html) with four slots under `cfg(kani)`.
Reject insertions exceeding the bound.
Keep production mutation calls and metadata inputs.
Compare fixture columns with native events.
Cover every replacement and parent-admission combination.
Separate successful transitions into link topology, untouched rows, generation, size, timestamp, and attribute obligations.
Derive unwind bounds from row and name geometry.
Prove insertion, lookup, and removal over arbitrary bounded entries, keys, and values.

Assume the standard library implements its documented map contract.
Reject iteration and storage observations in proofs.
Hashing, allocation, and memory layout remain external assumptions.

## Alternatives

Larger deadlines preserve the hashing problem.
Reducing metadata inputs weakens the required domain.
Changing production storage risks unrelated performance regressions.

## Confirmation

Require model proofs, index proofs, native differential operation sequences, and mutation controls using the production map.
Run applicable native integration and Miri checks.

## Consequences

Production representation and saved bytes stay unchanged.
Review the model when its map operations or proof scope changes.
