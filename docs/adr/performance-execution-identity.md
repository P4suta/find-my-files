---
date: 2026-11-05
---

# Performance execution identity

## Status

Proposed.
Partly supersedes [Measurement discipline](0013-measurement-discipline.md).

## Context

Processor names omit affinity and Rayon workers.
Invalid counters can invalidate a comparison.

## Decision

Admit one processor group with a nonempty process mask within the system mask.
Choose a positive worker count within assigned capacity and configure every child.
Record both masks and effective workers in measurement format 2.
Require exact baseline equality and unchanged assignment after measurement.
Retain source, dependency, compiler, clock, load, and case checks.
Require finite nonnegative counters and valid summaries for every phase.
Malformed rows reject the collection.
Boosted clocks above 100% remain valid.
Save original counter output before parsing.

## Alternatives

CPU counts permit different assignments.
Ambient worker requests leave child configuration unknown.
Older records omit execution identity.

## Consequences

Existing measurements need fresh records.
Additional groups require complete assignment and inheritance support.
[API analysis](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/performance-execution-identity.md) records Windows and Rayon assumptions.

## Confirmation

Run production proofs and native affinity, environment, baseline, and malformed-counter regressions.
Rejected measurements must never promote baselines.
Reconsider when supporting multiple processor groups.
