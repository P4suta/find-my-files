---
date: 2026-11-05
---

# Performance execution identity

## Status

Proposed.
This partly supersedes the baseline identity in [Measurement discipline](0013-measurement-discipline.md).

## Context

A performance comparison needs equal execution conditions for its baseline and candidate.
The processor description and an environment-provided CPU count omit the process affinity and Rayon worker count.
Assigning sixteen workers to four logical CPUs differs from assigning four workers to the same CPUs.
Matching clock and load summaries leave worker counts unknown.

## Decision

The performance runner reads the Windows process and system affinity masks and admits one processor group.
The process mask must contain at least one CPU and stay within the system mask.
The runner chooses a positive worker count within the assigned capacity.
Without an explicit request, it uses the number of assigned logical CPUs.
Malformed, zero, and excessive requests fail before measurement.
Every measured child receives the chosen count explicitly through `RAYON_NUM_THREADS`.
Windows supplies the child's inherited process affinity.
The runner checks the execution configuration again after measurement and rejects a changed assignment.

Measurement format 2 records the system mask, process mask, and effective worker count.
Baseline admission requires exact equality with the current configuration and rejects older or incomplete records.
The prepared overflow comparison carries the same execution identity.
The source, dependency, compiler, processor, clock, load, case inventory, and promotion checks remain in force.
Engine contracts and saved index formats remain unchanged.

The [Windows process affinity API](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getprocessaffinitymask) defines the process mask and its processor-group limitations.
[Rayon thread count](https://docs.rs/rayon/1.12.0/rayon/struct.ThreadPoolBuilder.html#method.num_threads) defines the environment-controlled default pool.
The runner requires one processor group because a primary-group mask can omit CPUs in other groups on the supported Windows versions.

## Consequences

Existing baseline and prepared-comparison records need a fresh measurement or preparation.
The performance harness supports one processor group while the app keeps its existing Windows support range.
Support for further groups requires a complete assignment identity and the same inheritance and comparison checks.
The pure execution planner has a production-bound scalar harness.
Native tests cover baseline rejection, child environment selection, and inherited Windows process affinity.

Older readers accept incomplete records when the old format makes execution fields optional.
Recording only the number of assigned CPUs would permit comparisons on different CPU sets.
Recording an ambient worker request without configuring the child would leave the measurement dependent on default pool selection.
