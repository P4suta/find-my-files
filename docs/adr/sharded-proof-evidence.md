---
date: 2026-11-05
---

# Sharded proof evidence

## Status

Proposed.
This refines execution of the [source-bound verification](source-bound-verification.md) inventory.

## Context

The decoder harnesses partition the admitted bytes without removing malformed inputs.
The hosted run for `7a1ec5e` reached its 60-minute job limit before completing the inventory.
Completed cases passed, but the job produced no complete receipt or Miri result.
The job deadline bounds resource use and establishes no proof verdict.

## Decision

Keep the complete inventory and partition its ordered package and harness names by ordinal modulo the partition count.
The maintained runner accepts between one and four partitions.
Invalid indexes fail validation.
Hosted verification uses four jobs with one solver worker in each job.
Each job retains the existing harness watchdog and job deadline.
Source binding and the existing assumptions, unwind bounds, verifier pin, and assertion checks remain required.

Kani receives the selected fully qualified names with `--exact` and repeated `--harness` options.
The pinned [Kani argument definitions](https://github.com/model-checking/kani/blob/kani-0.68.0/kani-driver/src/args/mod.rs) define those selection semantics.
Every original report must contain exactly its assigned nonempty harness set and successful assertions and cover properties.
A partition receipt binds the original report bytes to the complete source fingerprint.
Its scope prevents a partial result from claiming complete verification.

Collection requires every declared partition index, the same partition count and source fingerprint, and unchanged report bytes.
The combined receipt binds the original partition receipts without synthesizing a solver result.
Verification rechecks the complete partition set and original reports.
Missing, duplicated, stale, unsuccessful, vacuous, or changed evidence fails verification.
The production partition predicate has a Kani harness for unique ownership of every machine-sized ordinal under the supported partition counts.
Native tests exercise receipt acceptance and refusal through the actual collector and verifier.

The required `xtask` status depends on the fast tooling job and every requested proof job.
A requested job must succeed before proof collection.
The `Miri` interpreter runs after complete proof collection.
The existing single-run command remains available and requires the same complete inventory.

Increasing a job deadline would keep one long failure and retry unit.
Removing harnesses or accepting partial logs would leave assessed properties unverified.
The chosen partitioning preserves the admitted domain and permits independent bounded execution.

## Consequences

Hosted verification uses at most four proof runners and repeats package compilation across partitions.
The complete receipt depends on every partition and remains invalid when its source or configuration changes.
A timeout still leaves missing evidence.
Reconsider the partition limit when the registered inventory no longer fits the existing safety budgets.
