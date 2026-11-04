---
date: 2026-11-04
---

# Release overflow checks

## Status

Proposed.

## Context

Before this decision, release builds enabled overflow checks for `fmf-contract`, `fmf-proto`, `fmf-ffi`, and `fmf-service`.
The core and runtime inherited the default release behavior.
The reassessment in [issue 152](https://github.com/P4suta/find-my-files/issues/152) includes the split core and runtime and preserves the existing measurement discipline in [ADR-0013](0013-measurement-discipline.md).

The November 4 experiment used one clean source revision, the pinned compiler, and three preserved executables after an owner-controlled restart.
Variant A retained the boundary-only profile.
Variant B added checks to `fmf-core` and `fmf-runtime`.
Variant C enabled checks throughout the release profile.
Source revision 0692a260cba912dc84c5bab85c5e30bae22db172 used Rust 1.99.1 on Windows build 26300 with sixteen logical processors.
Five rounds alternated A/B/C and C/B/A for fifteen valid real-volume measurements.
The existing evaluator accepted all twenty comparisons with twenty-four required cases per comparison.
The controls include each round's A compared with the first round's A.

The per-query median p50 changes for C range from a 1.38% decrease to a 5.34% increase relative to A across ten distinct query cases.
The median change for `e` is a 0.53% decrease.
The median change for `a` is a 1.45% increase.
C's largest single paired query p50 increase is 19.29%, for `win` in round one.
B's largest single paired query p50 increase is 30.98%, for `win` in round five.
The measurements support choosing C within the existing budgets.
They don't establish a universal cost or exclude smaller regressions on other corpora.

The first attempt exceeded the postflight CPU limit and remains incomplete.
It contributes no accepted comparison to the five-round result.
The complete experiment receipt has SHA-256 2e99bde9d841a215de53352dfc49e9b0def1c41b62e6c3d89349df32afd4ca6b and records the reports, executable identities, and compiler arguments.

## Decision

Enable `overflow-checks = true` in the engine workspace's `[profile.release]`, matching measured variant C.
Prefer C's wider coverage to A and B because C met every existing performance gate on the measured corpus.
Keep `panic = "unwind"`, the explicit boundary overrides, and the existing Windows Control Flow Guard configuration.
Keep the performance thresholds and measurement requirements unchanged.

The [Cargo profile documentation](https://doc.rust-lang.org/cargo/reference/profiles.html#overflow-checks) defines the option as the compiler's runtime integer-overflow control.
An overflow detected by those checks triggers a panic.
Use the compiler checks alongside explicit arithmetic invariants and the production-bound proofs.

## Consequences

The release profile adds checks to the core, runtime, and the remaining crates that inherit its settings.
Measured variant C passed the existing evaluator on the recorded corpus.
Each changed candidate requires source-bound proofs, compatibility checks, an adjacent baseline, and the release performance gate.
Reassess the profile when the pinned compiler, representative corpus, or measured performance changes.
