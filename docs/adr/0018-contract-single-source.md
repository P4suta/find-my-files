# ADR-0018: Contract single source of truth (fmf-contract) + capture-first golden corpus

Date: 2026-06-11 / Status: Adopted

## Context

Duplicated wire definitions drift.
A dynamic library can depend on a Rust library.
[Historical analysis](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/0018-contract-single-source.md) preserves migration evidence.

## Decision

The dependency-free fmf-contract crate owns constants, layouts, and pure byte conversions.
It generates checked-in C# bindings.
Both languages consume the same captured contract/golden/ bytes.
Intentional changes edit the contract, recapture with FMF_BLESS=1, run just contract-gen, and pass both language suites in order.
Error codes remain append-only.
Preserve the fmf_engine library name and external formats.
Keep only SnapshotStore and JournalSource as engine seams.
[Pure core and shared runtime](pure-core-and-shared-runtime.md) supersedes complete-engine placement.

## Alternatives

Duplicated constants detect drift after publication.
A corpus generated from changed code can reproduce generator errors.

## Consequences

Explicit generation adds a contributor step.
Drift and shared golden tests reject stale bindings or changed bytes.

## Confirmation

Run contract generation, Rust and C# golden tests, and architecture guards.
Reconsider the seam limit after a demonstrated regression confined to administrator-only paths.
