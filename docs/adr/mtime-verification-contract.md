---
date: 2026-11-07
---

# Timestamp projection verification

## Status

Proposed.

## Context

Metadata proofs repeat timestamp conversion across heap storage.
Standalone projection atomics corrupt empty-vector capacity in the pinned verifier.

## Decision

Prove the actual converter, result uniqueness, and projection refinement for every ordered pair of signed 64-bit timestamps.
Use arbitrary unsigned 32-bit results.
Equal inputs share one result.
Reject calls outside the registered pair.
The projection uses one uniquely initialized allocation.
Verify the sequential adapter before [stubbing](https://model-checking.github.io/kani/reference/experimental/stubbing.html) the converter in metadata proofs.
Native tests keep the real converter.
Verify stored-column equality and its safe, deterministic decoder.
Require converter, projection, adapter, and metadata proofs for identical source and configuration.

## Alternatives

Restricted timestamps remove inputs.
Independent results discard function congruence.
Unchecked replacements lack refinement evidence.

## Confirmation

Require complete inventory validation, the vector-initialization isolation control, and assertion failures from production and model mutations.
Missing or stale evidence blocks completion.

## Consequences

Production behavior and saved bytes remain unchanged.
The projection admits more encoded pairs than the real converter.
Reconsider when callers introduce another timestamp or change encoding.
