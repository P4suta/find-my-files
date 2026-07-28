# ADR-0046: The change-to-screen path is one allocated latency budget

Date: 2026-07-27 / Status: Accepted

## Context

"A filesystem change is visible in the result list within 1 second" is an
acceptance criterion, but no single component owns it. The path crosses four
owners — the USN tail loop, the volume worker's batch apply, the engine's event
debounce, and the UI's re-query plus render. Each stage's own number exists in
code (a park duration, a debounce interval, a test budget), and each is
individually defensible. What has never been recorded anywhere is that they are
**one budget**: that the stages must sum to less than the AC, how much of it
each stage is allowed to spend, and — most importantly — that adding a
plausible-looking delay in any one layer silently spends someone else's share.

The concrete failure this guards against is throttle accretion. Every layer on
this path has a locally reasonable argument for coalescing events ("the UI is
re-querying too often"), and a second throttle is invisible in that layer's own
tests while roughly doubling the observed end-to-end delay.

## Decision

Treat change → on-screen as a single budget, allocated once and in one place:

| stage | budget | owner |
|---|---|---|
| idle-edge USN discovery | ≤250ms | the tail loop's non-blocking-read park (**0 on a busy volume**, which never parks) |
| USN batch commit | ≤100ms | volume worker |
| IndexChanged debounce | 200ms | engine |
| UI re-query | ≤100ms | app |
| render | ≤100ms | app |
| **total** | **≤750ms worst case** | (≤500ms once the volume is active) |

Two invariants follow, and they — not the individual numbers — are the decision:

1. **Exactly one event-rate throttle exists on the whole path, and it is the
   engine-side IndexChanged debounce.** No additional throttle, coalescing
   timer, or "settle" delay may be added on the UI side or at the transport.
   The debounce is placed in the engine because that is the single point where
   every path (in-proc FFI and pipe push alike) already converges; a second one
   downstream would be uncoordinated with it and could only ever add.
2. **The budget is allocated, not accumulated.** A stage that needs more must
   take it from another stage in this table and this ADR must be updated. A
   change that adds a stage-local delay without a corresponding reduction
   elsewhere is a regression against the AC even when every stage-local test
   still passes.

The pipe transport adds no new stage. Its page round trip is charged against
the existing re-query allowance: ResultPage 64-row round trip **p99 ≤5ms**,
which is enforced by the loopback integration test and continuously observed as
`PageRttEwma` in the diagnostics panel. Event push is one hop after the
debounce above, so the budget structure is identical on both transports — the
service split does not cost a stage.

## Consequences

- The 200ms debounce cannot be tuned in isolation to "reduce flicker"; flicker
  is addressed structurally instead, by the unchanged-result in-place refresh
  path (ADR-0015), which redraws nothing rather than by delaying the update.
- Because the largest single term is the idle-edge park, worst case is only
  observed on an otherwise quiet volume; the active-volume figure (≤500ms) is
  the one users normally experience, and quoting the worst case is the
  conservative choice on purpose.
- Regressions here are not caught by any one component's tests. The
  transport-level term is the part with a mechanical gate (the ≤5ms loopback
  assertion); the rest is upheld by this allocation and by the single-throttle
  rule being reviewable.
- The numbers live here rather than being restated per component, so a stage
  cannot quietly redefine its own share.

## Re-examination triggers

- The AC itself changes (a tighter than 1s change-visibility requirement), at
  which point the whole table is re-allocated rather than one stage shaved.
- Pipe page-fetch p99 exceeds 5ms as the norm — this is the shared trigger with
  [ADR-0016](0016-service-split-named-pipe.md) / [ADR-0018](0018-contract-single-source.md),
  since a transport that no longer fits inside the re-query allowance breaks the
  "the split costs no stage" premise.
- A measured need for a second throttle (e.g. a device whose USN churn saturates
  the UI even after the engine debounce). Then the debounce moves or is
  re-parameterized — a second throttle is still not added.
