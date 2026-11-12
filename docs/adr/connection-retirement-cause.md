---
date: 2026-11-05
---

# Connection retirement cause

## Status

Proposed.

## Context

Request deadlines, reader failures, and supervisor cleanup can race.
Replacing their causes obscures the original failure.

## Decision

Each connection records its first non-null retirement cause through atomic comparison and exchange.
Publish the cause before clearing its public slot or disposing its stream.
Pending requests and admitted write failures use the recorded cause.
Retirement affects only the captured generation.
At completion, caller cancellation takes priority and retains its token; client disposal follows.

## Alternatives

A supervisor-only cause misses admitted writes.
A client-wide lock couples independent generations.
Sleeps cannot establish ordering.

## Consequences

One nullable reference belongs to each connection.
Native atomics, tasks, timers, and pipes remain external assumptions.
Contracts remain unchanged.

## Confirmation

Transport tests force retirement, deadline, cancellation, disposal, and replacement through barriers and a virtual clock.
Service integration remains required.
[Schedule coverage](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/connection-retirement-cause.md) describes the finite checks.
Reconsider when a maintained verifier covers native pipe semantics or retirement gains a separate decision core.
