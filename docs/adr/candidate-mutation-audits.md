---
date: 2026-11-05
---

# Candidate mutation audits

## Status

Proposed.
This extends the manual-audit decision in [ADR-0022](0022-boundary-seams-behavioral-tests.md).

## Context

The protected-main controller owns mutation policy, tool pins, accepted identities, and independent verification.
A pending architecture or runner migration can make the main inventory unsuitable for the candidate.
Pre-merge verification must exercise the complete candidate while preserving the Release trust boundary.

## Decision

The manual [mutation workflow](../../.github/workflows/mutants.yml) supports an explicit candidate-audit purpose.
The repository owner must dispatch a branch whose caller, workflow, controller, and target share one immutable commit.
The [execution-authority command](../../xtask/src/mutation_authority.rs) checks GitHub-provided identities and both the original and triggering actor before each mutation producer and the independent verifier.
Its [production-bound proof](../../xtask/src/mutation_authority_proofs.rs) checks that candidate and protected-main execution are disjoint for every admitted authority state.
GitHub authentication, ref metadata, runner isolation, and the authorization API responses remain external trust assumptions.
Regression tests check the adapter's commit, ref, event, and actor interpretation.

Both purposes use the same shard builders, exact inventories, accepted equivalents, and raw-evidence verifier.
Candidate evidence has a distinct artifact name and binds its exact run and controller.
The default purpose remains protected-main.
The Release workflow keeps that default and reruns the full campaign with its protected-main controller and Release run identity.
A candidate result grants no Release signing or publication authority.

Running the main controller for a candidate with incompatible policy omits intended coverage or fails before testing.
Changing Release authorization to accept branch evidence would weaken its trust boundary.
Merging the stack solely to exercise a verification tool would remove pre-merge evidence.

## Consequences

The complete Windows campaign can find defects before merge without occupying an owner's machine.
Release still repeats the campaign on the exact Release source.
The owner must review candidate-controlled policy as part of the candidate diff.
The immutable controller and raw-artifact binding identify the policy that each audit used.
GitHub selects the caller's commit for a [same-repository reusable workflow](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows).
GitHub supports [manual workflow dispatch from a branch](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).
