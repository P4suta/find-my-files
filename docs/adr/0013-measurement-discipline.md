# ADR-0013: Measurement discipline (cold machine, back-to-back, real-volume absolute gate)

Date: 2026-06-11 / Status: Accepted

## Context

Thermal drift and background work can outweigh code changes.
[Recorded measurements](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/0013-measurement-discipline.md) explain the thresholds.

## Decision

Use cold, idle hardware and native preflight, monitoring, and postflight.
Require preflight processor performance of at least 95% and idle CPU means at most 20%.
Compare loaded clock within five percentage points and additional CPU within twenty points of the baseline.
Require matching source, dependencies, compiler, processor, and [execution configuration](performance-execution-identity.md).

Real-volume ceilings remain 8 seconds at 250,000 entries or 60 seconds at one million, 110 bytes per entry, query p99 50 milliseconds, and restore p50 one second.
Query p50 allows at most 50% regression.

## Alternatives

Stored Criterion comparisons confound time with code.
Use adjacent interleaved code versions; informational comparisons retain the 10% confidence-bound threshold.

## Consequences

Invalid measurements cannot promote baselines.
[ADR-0048](0048-direct-dispatch-release-workflow.md) retires hosted measurement orchestration.

## Confirmation

Run just perf-gate before merging core or runtime changes and before release signing.
Reconsider the drift band after changed hardware or reproducible clock differences.
