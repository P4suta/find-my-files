---
date: 2026-10-20
---

# Source-bound verification for byte and layout boundaries

## Status

Proposed.

## Context

Client and disk bytes cross privilege boundaries.
Tests sample executions, and untracked fuzz inputs lose regressions.

## Decision

Kani harnesses call production conversions and validators with explicit bounds, assertions, and reachable covers.
The inventory binds harnesses to source, dependencies, tools, and runners.
Reject missing, failed, incomplete, unreachable, or stale results.
Miri checks applicable contract boundaries.
Initialize snapshot buffers before safe reads.
Commit fuzz regressions and replay them before bounded exploration.
Keep native integration for Windows APIs, privileges, and scheduling.

## Alternatives

Detached models do not verify production code.
Longer fuzzing without preserved seeds loses reproducibility.
Compilation establishes no behavioral property.

## Consequences

Small pure helpers may expose scalar invariants.
Native behavior, formats, generated contracts, mutation scope, and reviewed identities remain unchanged.

## Confirmation

Run [verification gates](../verification.md) and ensure missing reports and broken invariants fail.
[Original scope](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/source-bound-verification.md) records the boundaries.
Reconsider if verifier semantics or required native boundaries change.
