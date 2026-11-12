---
date: 2026-11-05
---

# Candidate mutation audits

## Status

Proposed.
Extends [ADR-0022](0022-boundary-seams-behavioral-tests.md).

## Context

Runner migration can make protected-main mutation policy unsuitable for a candidate.
Pre-merge audits must preserve Release authority.

## Decision

Candidate audits require the authenticated repository owner and identical caller, workflow, controller, and target commits.
Check authority before every producer and independent verifier.
Keep protected-main as the Release default.
Both purposes share exact inventories, tools, survivor identities, shard builders, and raw-result verification.
Require a passing baseline, complete outcomes, consistent process exit, and no timeout.
Candidate artifacts bind their run and controller and grant no signing authority.

## Alternatives

An incompatible controller omits intended coverage.
Accepting branch evidence for Release weakens its trust boundary.
Merging solely for verification loses pre-merge testing.

## Consequences

Candidate audits add a secretless manual route.
GitHub identities, authentication, and runner isolation remain trust assumptions.

## Confirmation

Production proofs check disjoint purposes and outcome consistency.
Regressions reject mismatched actors, refs, commits, and incomplete reports.
[Execution details](https://github.com/P4suta/find-my-files/blob/198680fcf43d9de21d7be88c4cd270146af94a2e/docs/adr/candidate-mutation-audits.md) retain the analysis.
Reconsider when candidate policy no longer differs from main.
