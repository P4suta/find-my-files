---
date: 2026-11-07
---

# Checked arithmetic verification

## Status

Proposed.

## Context

The C Bounded Model Checker (CBMC) 6.11.0 reverses flattened overflow-result fields in its SMT encoding.
A generated timestamp counterexample contradicts native execution.
Upstream [issue 9144](https://github.com/diffblue/cbmc/issues/9144) identifies the representation error.

## Decision

Use the pinned Z3 release and CBMC's datatype encoding for timestamp contracts.
Preserve the Kani toolchain, production arithmetic, and input domains.
Verify the archive checksum and executable version locally and in CI.
Require native counterexample playback and checked-arithmetic regression controls before accepting the solver route.

## Alternatives

Bitwuzla requires the affected flattened representation.
The upstream CBMC correction has no stable release yet.
Disabling arithmetic checks removes required evidence.

## Confirmation

Require converter and uniqueness proofs, planted assertion failures, and complete source-bound proof collection.
Reject absent or altered tools before verification.

## Consequences

Provisioning includes a second SMT solver.
Saved affected SMT verdicts supply no proof evidence.
Reconsider solver selection when Kani includes the released upstream correction.
