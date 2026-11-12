---
date: 2026-11-08
---

# Reject static mutation verdicts

## Status

Proposed.

## Context

The pinned Stryker Microsoft Testing Platform (MTP) runner reuses initialized static values across mutations.
Native controls miss the Explorer filename mutation after another mutation but detect it in a fresh process.
[Upstream reports the lifecycle defect](https://github.com/stryker-mutator/stryker-net/pull/3695).

## Decision

Both acceptance parsers reject executed static initialization verdicts, including reported kills.
`ShellOps.ExplorerPath` computes the absolute Windows path on use.
The shared configuration runs one mutation at a time because [concurrent MTP runs can misattribute outcomes](https://github.com/stryker-mutator/stryker-net/issues/3832).
Reviewed targets, operators, equivalent identities, and time budgets remain unchanged.

## Alternatives

A fresh-process runner would preserve cached initialization but needs another integration.
The VSTest control doesn't activate mutations under xUnit v3.
A private runner fork would add a toolchain outside the stable dependency pins.

## Consequences

Opening and revealing targets perform a folder lookup on each operation.
Static initialization requires a supported runner or an uncached representation before acceptance.
Reconsider after a pinned stable runner passes the native lifecycle controls.

## Confirmation

The reused-process control must detect the filename mutation.
Both parsers must reject static verdicts and accept ordinary reports.
The complete managed baseline, source gates, and mutation campaign must pass.
