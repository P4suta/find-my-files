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

The four-partition run for `f3a8836` lost every hosted runner during an offset-80 resident admission case.
Each active case stopped before its harness watchdog.
The logs contain no counterexample or completed verdict for the active cases.
The shutdown logs contain no measured memory-exhaustion event.

With the process limit, the next run uploaded original unsuccessful reports and completed normal cleanup.
The offset-80 resident invariant report contains solver errors without a failed assertion or completed cover checks.
The run produced no complete receipt or Miri result.
The following regular-output run confirms solver memory exhaustion during propositional reduction.
Earlier cases pass, but the incomplete obligations at the earliest offsets still grant no proof credit.
The second-length partitions still exhaust solver memory when two attributes leave room for a third header.
The decoder harnesses partition that final length into fitting constants and their complete complement.
Both admission and invariant obligations preserve every original byte input.
Constant final lengths alone still exhaust solver memory in a three-header geometry.
The same cases also partition the later residency markers while keeping every marker pair and every third length.

## Decision

Keep the complete inventory and partition its ordered package and harness names by ordinal modulo the partition count.
The maintained runner accepts between one and four partitions.
Invalid indexes fail validation.
Hosted verification uses four jobs with one solver worker in each job.
Each job retains the existing harness watchdog and job deadline.
Source binding and the existing assumptions, verifier pin, and assertion checks remain required.
The [decoder proof](decoder-bounds-and-progress.md) derives each chain unwind bound from its possible advances and final returning probe.
An insufficient bound must fail an unwinding assertion.

The inventory also bounds virtual address space per process.
Every admitted limit is at most `8 GiB`.
The runner invokes Kani through [`prlimit`](https://man7.org/linux/man-pages/man1/prlimit.1.html).
Child processes inherit the selected limit.
The limit applies to each process rather than the combined resident memory of the job.
The runner records [free](https://man7.org/linux/man-pages/man1/free.1.html) memory observations every ten seconds for at most one hour.
It owns and reaps the telemetry process on ordinary completion or error.
Forced runner termination can prevent cleanup and final observations.
Telemetry supplies diagnostic observations and never a proof verdict.
Regular verification output preserves diagnostics for unsuccessful checks.
Kani stops after the first unsuccessful harness with `--fail-fast`.
The runner accepts a receipt only after every assigned harness passes.

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
