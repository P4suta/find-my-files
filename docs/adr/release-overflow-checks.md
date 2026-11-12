---
date: 2026-11-04
---

# Release overflow checks

## Status

Proposed.

## Context

Boundary crates enabled overflow checks.
Core and runtime inherited release defaults.
Reassessment follows [issue 152](https://github.com/P4suta/find-my-files/issues/152) under [ADR-0013](0013-measurement-discipline.md).

## Decision

Enable `overflow-checks = true` throughout `[profile.release]`, matching measured variant C.
[Cargo](https://doc.rust-lang.org/cargo/reference/profiles.html#overflow-checks) defines runtime overflow checks that panic.

## Alternatives

A retains boundary-only checks.
B also covers core and runtime.
Prefer C because every existing gate passed across five alternating rounds and fifteen runs.
C's per-query median p50 changes ranged from a 1.38% decrease to a 5.34% increase.
Its worst paired query increased 19.29%.

## Consequences

Preserve unwind panics, boundary overrides, Control Flow Guard, and thresholds.
Measurements support C on the recorded corpus, without guaranteeing universal cost.
Reassess when the compiler, corpus, or performance changes.

## Confirmation

Clean source 0692a260cba912dc84c5bab85c5e30bae22db172 used Rust 1.99.1, Windows build 26300, and sixteen logical processors after an owner-controlled restart.
Twenty comparisons passed twenty-four required cases each.
The rejected first attempt contributes nothing.
Receipt SHA-256: 2e99bde9d841a215de53352dfc49e9b0def1c41b62e6c3d89349df32afd4ca6b.
Each candidate requires proofs, compatibility checks, an adjacent baseline, and the performance gate.
