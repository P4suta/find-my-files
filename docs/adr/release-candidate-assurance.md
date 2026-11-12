---
date: 2026-11-12
---

# Release candidate assurance

## Status

Proposed.

## Context

Full proofs and interpreter checks repeat across ordinary stack updates.
Weekly fuzzing repeats exploration without source changes.
Owner policy assigns expensive verification to Release candidates.

## Decision

Keep formatting, lint, unit tests, integration tests, and inventory checks on pull requests and merge groups.
Reuse CI for full proofs, interpreter checks, and fuzzing at the Release stage.
Every checkout uses the admitted candidate commit.
Require full verification before staging signatures.
Keep mutation gates, protected environments, and manual fuzz execution.
Remove periodic fuzz execution and duplicate CI push triggers.

## Alternatives

Full verification on every pull request detects intermediate defects earlier but repeats expensive campaigns.
Manual checks alone provide no enforced publication boundary.

## Consequences

Intermediate commits can await full assurance until candidate verification.
Reassess when defects escape fast checks or candidate verification becomes too slow.

## Confirmation

Keep inventory and report rejection tests.
Exercise failed, skipped, and missing prerequisite fixtures in the summary gate.
Inspect candidate checkout identities and successful full verification before signing.
[GitHub job dependencies](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idneeds) enforce the signing prerequisite.
