---
date: 2026-11-05
---

# Decoder bounds and progress

## Status

Proposed.
Refines [Pure core and shared runtime](pure-core-and-shared-runtime.md).

## Context

Repeated geometry checks obscure admission.
Mutable offsets can fail to progress, and duplicate surrogate arithmetic can diverge.

## Decision

Consume validated ranges and nonzero widths through remaining slices.
Preserve ordering, alignment, unique IDs, complete termination, accepted values, and atomic output rejection.
Share checked geometry and surrogate conversion.
Track four high attribute IDs inline, then spill without limiting input size.

Production proofs compare admission with an independent bounded reference.
Disjoint constructive partitions preserve complete original field domains.
Retain assertions, covers, unwind checks, and resource limits.
Use pinned solvers without treating deadlines as success.

## Alternatives

Repeated guards preserve ambiguity.
Timeout-based tests miss progress invariants.
Reducing domains or weakening assertions manufactures confidence.

## Consequences

Constructors, classifiers, and inline storage add complexity.
External formats and supported inputs remain unchanged.
[Proof construction and attempts](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/decoder-bounds-and-progress.md) preserve the analysis.

## Confirmation

Run complete proofs, malformed native fixtures, generated ID histories, fuzz replay, and mutation checks at their required stages.
Reconsider after an admission mismatch, lost outcome cover, or changed proof geometry.
