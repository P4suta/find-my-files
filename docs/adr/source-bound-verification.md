---
date: 2026-10-20
---

# Source-bound verification for byte and layout boundaries

## Status

Proposed.

## Context

The service decodes client messages and disk structures across privilege boundaries.
Native tests and fuzzing exercise particular executions.
They don't establish properties for every admitted input.
Untracked fuzz inputs also discard regressions between CI runs.

The product remains Windows-only.
Portable verification follows the parser boundary established by [ADR-0047](0047-ntfs-grammar-outside-the-windows-gate.md).
The contract crate remains the only authority for wire types and byte conversions.

## Decision

Kani harnesses call the production byte conversions and layout validators directly.
The checked [inventory](../../verification/proofs.json) declares the required harnesses and their execution budget.
Each harness supplies an explicit unwind bound, assertions, and reachable cover cases.
The gate rejects missing harnesses, failed checks, incomplete runs, unreachable cover cases, and stale evidence.
Evidence binds the results to source files, dependency locks, tool configuration, and the runner implementation.

Miri executes the applicable contract tests with its isolation and borrow checks enabled.
Snapshot byte views use `bytemuck::Pod`, and the reader initializes each chunk before passing it to a safe `Read` implementation.
This keeps raw byte access inside the dependency's checked API and preserves initialized prefixes during buffer growth.
Windows APIs, process scheduling, and privileges remain native integration boundaries.
[Verification scope](../verification.md) records the distinction between these claims.

Fuzz targets keep regression inputs in the repository and replay them before bounded exploration.
Round-trip and metamorphic properties check decoded values where the format permits an inverse.
The existing CI jobs enforce these checks without adding a runner matrix.

Comments explain public contracts, safety, legal requirements, tool directives, or constraints that types and executable checks can't express.
Ordinary narration and source history belong outside the implementation.
Prose uses one sentence per line.

## Consequences

Pure validation may need a small internal function so the verifier can cover its complete scalar input domain without modeling unrelated standard-library machinery.
The public API, wire layout, persistence format, and native behavior keep their existing contracts.
The mutation baseline keeps its operators, replacements, and rationale when comment edits move source positions.

Detached models don't substitute for a production-code proof.
Increasing fuzz duration doesn't substitute for preserved regressions or reachable success paths.
Passing a bounded harness establishes its stated input domain.
It doesn't establish correctness of every engine function or native adapter.
