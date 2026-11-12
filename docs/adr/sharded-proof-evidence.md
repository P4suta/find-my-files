---
date: 2026-11-05
---

# Sharded proof evidence

## Status

Proposed.
This refines [source-bound verification](source-bound-verification.md).

## Context

The decoder inventory outgrew one hosted job.
Later resident-record proofs exhausted solver memory.
Incomplete logs establish no verdict.
The [decoder decision](decoder-bounds-and-progress.md) preserves every admitted byte input through complete partitions.

## Decision

Partition ordered package and harness names by ordinal modulo the partition count in the [maintained runner](../../xtask/src/proof.rs).
Keep its declared partition, worker, watchdog, memory, and job limits.
Before solver execution, require compiled Linux harness names to match the inventory exactly.
Reject absent, unexpected, duplicated, empty, inconsistent, or incompatible discovery reports.
`just proof-discover` runs compilation checks without granting proof credit.

Bind each partition's original report bytes and scope to the complete source fingerprint.
Collect every partition once and revalidate the original reports.
Reject missing, stale, unsuccessful, vacuous, or changed evidence.
Run Miri after complete collection.
The single-run command requires the same complete inventory.

## Alternatives

Longer deadlines keep one large failure unit.
Dropping harnesses removes assurance.

## Confirmation

Native tests check refusal cases.
Kani checks the partition and acceptance predicates.

## Consequences

Partitioning repeats compilation but bounds each retry unit.
Timeouts and telemetry supply no proof credit.
Reconsider partition limits when the inventory exceeds existing budgets.
