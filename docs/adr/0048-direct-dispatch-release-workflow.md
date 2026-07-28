# ADR-0048: The release workflow is dispatched directly, not reached through workflow_run

Date: 2026-07-28 / Status: Accepted (amends [ADR-0029](0029-ci-signing-cka-pipeline.md); retires the CI measurement chain in [ADR-0013](0013-measurement-discipline.md) and [ADR-0035](0035-automated-versioning-with-release-please-and-build-channels.md))

## Context

Publication used to be four stages deep:

```
workflow_dispatch → performance-gate-request
  → (workflow_run) → performance-controller     ← requires a self-hosted JIT runner
  → (workflow_run) → performance-release
  → (workflow_call) → release
```

The reasoning was sound and is worth restating, because the shape is right for
the world it was designed for. `workflow_dispatch` loads workflow YAML from the
*selected ref*, so a dispatchable workflow must never contain a self-hosted job:
anyone who can push a branch could otherwise define one. The measurement runner
is not disposable, so that is a real escalation. Splitting into a hosted-only
request plus a default-branch `workflow_run` controller closes it correctly.

Two facts overtook it.

**The chain is unstartable.** `performance-controller.yml` targets a restricted
*organization* runner group, `fmf-performance`, and this is a user-owned
repository where such a group cannot be created. `just performance-doctor`
failed by design for exactly that reason, as its own recipe comment said. The
chain therefore stopped at stage one, and `release.yml` was never reachable
through it. (`main`'s older, pre-chain `release.yml` did publish v0.1.0 and
v0.1.1; what has never been reachable is this four-hop arrangement.)

**The cost is seven CodeQL alerts per PR.** `actions/cache-poisoning/poisonable-step`
(`CachePoisoningViaPoisonableStep.ql`) requires an externally triggerable event
that either satisfies `runsOnDefaultBranch` or is a `workflow_call` from a
caller that does. `runsOnDefaultBranch` is a literal list of 21 event names in
`CachePoisoningQuery.qll`, and `workflow_run` is on it. Every live alert binds
`(workflow_run)` through caller resolution up to `performance-release.yml`.
CodeQL cannot follow data across workflow boundaries — the input *is* in fact
constrained, since the dispatcher verifies the source run's conclusion, event,
head branch, head SHA, workflow ID, and repository before passing it — and it
never will. Seven false alerts on every PR drown the real ones; they were the
only failing required check on PR #164.

`workflow_dispatch` is not in that list, is not `push`, is not
`pull_request_target`, and is not `workflow_call`. Removing the trigger removes
the alerts because the construct is gone, not because they were dismissed.

## Decision

`release.yml` becomes a `workflow_dispatch` from the default branch, taking
`tag_name`, `commit_sha`, and `release_id` as required string inputs.
`release-please.yml`'s renamed `dispatch-release` job — still the place where
`release_id` is derived from release-please's own documented `upload_url` — runs
`gh workflow run release.yml --ref main` with that triple.

Four things carry the security property the chain used to carry.

1. **Dispatching with `--ref main` loads main's YAML.** A tag identifies build
   data, never workflow code, directly rather than through two hops.
2. **A new secretless `preflight` job is admission control.** Before any other
   job starts it re-derives and asserts the whole identity: exact `vX.Y.Z` tag
   shape, `GITHUB_REF` is `refs/heads/main`, `GITHUB_EVENT_NAME` is
   `workflow_dispatch`, controller and workflow SHAs are equal 40-hex commits on
   main's lineage, `commit_sha` is 40-hex, the tag resolves *live* to
   `commit_sha`, the tag is on main's lineage, and `release_id` is the exact
   numeric non-prerelease draft whose `target_commitish` is that tag SHA.
3. **Every later job keeps revalidating for itself.** `build`, `sign-stage`,
   `package`, and `publish` each repeat the draft-identity check they already
   had. `preflight` narrows the window; it does not become the single point the
   rest of the pipeline trusts.
4. **The environments are the secret boundary for an off-main dispatch.** Both
   `release` (eSigner secrets, required reviewers) and `release-please` (App
   credentials) restrict deployments to protected `main`. Selecting a non-main
   ref would run that ref's copy of this file, and that copy would reach no
   signing credential, no App token, and no publication authority.

`github.triggering_actor` is asserted against `^[A-Za-z0-9-]+(\[bot\])?$`,
written to the run summary and a `::notice::`, and attested. Under the chain,
"who started it" was structurally irrelevant; a dispatch makes it the primary
authorization fact, and an unrecorded primary fact is not a control.

### Authorization accounting

The 37 authorization checks in the old path were classified individually: **22
ported, 15 dropped.** Every drop is specific to the instrument being removed —
the `workflow_run` source-run identity checks, the evidence-artifact download
and re-verification (~460 lines of Python), the `display_title` parsing, the
organization runner-group assertions, and `performance_run_id` itself. Every
universal identity binding survives: ref is `refs/heads/main`, the tag resolves
live to `commit_sha`, both on main's lineage, `release_id` is the exact numeric
draft with matching `target_commitish`, the pre-publication revalidation, the
monotonic-version policy, and the fail-closed signing-secret behaviour. The
ported checks cost zero new lines in `build`, whose first step was already a
verbatim superset of them.

### The attestation changes meaning, so it changes version

The custom release predicate goes to **`schemaVersion: 2`**. The predicate-type
URL is unchanged, so consumers must read `schemaVersion`:

- `release.performanceRunId` is **removed**; the run it identified no longer exists.
- `controller.triggeringActor` is **added**.
- `controller.runId` and `controller.runAttempt` **change referent.** Under
  schemaVersion 1 they identified `performance-release.yml`'s `workflow_run`
  run; they now identify `release.yml`'s own dispatch run. The field names are
  identical and the values are equally well-formed, which is exactly why this is
  recorded rather than left to be inferred — a silent referent change in a
  signed attestation is the drift class this ADR exists to remove.

### Threat-model delta, stated plainly

`release.yml` goes from **unstartable by anyone** to **startable by anyone
holding `Actions: write` on this repository**. That is a real widening and it is
accepted deliberately.

What an unauthorized dispatch can reach: `preflight`, and — only if it supplies
a genuine tag, its exact commit, and the matching numeric draft ID, all three
already on protected main — `build` (60-minute timeout), `sbom`, and `mutation`
(16 shards, 360-minute timeout). All three execute **before the first human
approval**. That is compute, not authority: none of those jobs holds a secret,
a write token, or an environment.

What it cannot reach: `sign` and `publish-approval` sit behind the
required-reviewer `release` environment, and `publish` behind
`release-please`. Both are restricted to protected `main`.

`preflight` is the mitigation for the compute exposure: a dispatch that cannot
name a real tag/commit/draft triple dies in a five-minute ubuntu job. The
`release-stable-publication` concurrency group (`cancel-in-progress: false`) and
release-please.yml's title-based dedupe prevent a second run for a triple that
already has one.

### `prevent_self_review = true` is rejected

The obvious hardening for "the dispatcher should not approve their own release"
does not apply here and would brick every release. Verified live: the `release`
environment has exactly one reviewer (P4suta), and the dispatching actor is that
same person — either directly, or as the identity behind the release-please App
that cuts the tag. Enabling `prevent_self_review` would make every release
unapprovable by the only person able to approve it. It is rejected explicitly
rather than listed as a consideration, and only becomes available when a second
maintainer exists (which is also `CODEOWNERS`' condition; see `docs/RELEASING.md`).

## Consequences

- **The real-volume performance gate becomes a human step.** `just perf-gate`,
  run on the reference machine before approving `sign`, replaces a mechanical
  precondition that could not start. The trade is an unreachable automated gate
  for a reachable manual one; the intent lives in DEV-287's elevated-session
  checklist. `docs/RELEASING.md` carries the step.
- **The Criterion/real-volume baseline is recorded locally.** `just bench-baseline`
  writes `engine/benches/baseline.json` on the reference machine and it lands
  through an ordinary reviewed PR. The hosted validate-then-propose split, the
  `P:` volume, and the baseline-writer App token are gone with the workflows.
- **`xtask/src/performance_doctor.rs` is deleted.** It audited live GitHub state
  for the instrument — runner-group membership, `restricted_to_workflows`,
  `selected_workflows`, `can_admins_bypass`, the `fmf-jit-ephemeral` label — and
  has nothing left to audit. `just performance-doctor` goes with it.
- **The `performance` environment and the `fmf-performance` runner group never
  existed.** Nothing needs to be torn down; the documentation that described
  provisioning them is removed rather than marked stale.
- **Two release guard tests are deleted and one is rewritten.** The replacement
  pins the new shape, including a repo-wide sweep asserting that no workflow
  carries a `workflow_run:` trigger and that the only `dangerous-triggers`
  suppression left is the unrelated `pull_request_target` auto-merge guard.
- The four deleted workflows remain in git history if an organization migration
  ever makes them relevant again.

## Verification

- **CodeQL**: the query's `where` clause cannot bind `workflow_dispatch` through
  either disjunct, so `release.yml` cannot match at all. The same query against
  the default ref already returns `[]`; the seven alerts on `refs/pull/164/merge`
  all bind `(workflow_run)` through the deleted caller.
- **zizmor** (`ci.yml:95`, a leaf of the required `ci-required` check) has its
  own cache-poisoning audit. Its
  `triggers_used_when_publishing_artifacts` recognises only `release`,
  tag-filtered `push`, and release-branch `push`; `workflow_dispatch` falls to
  the empty arm. The trigger swap does not trade one finding for another, and
  the `zizmor: ignore[dangerous-triggers]` suppressions the chain required are
  deleted rather than moved.

## Residual risk

If a future CodeQL bundle adds `workflow_dispatch` to
`defaultBranchTriggerEvent()`, the alerts return. `codeql-action` is SHA-pinned,
so that can only arrive through a reviewed Dependabot bump. The disposition
*then* is dismissal with evidence, not restructuring: grepping `cache` across
all six composite actions, `release.yml`, and `mutation-controller.yml` returns
exactly one hit, a prose comment. There is no `actions/cache`, no
`Swatinem/rust-cache`, and no `setup-*` invoked with a cache input anywhere on
the release path, so the rule's premise does not hold on this lane.

## Re-examination triggers

- **The repository migrates to an organization** and a hardened runner group
  becomes creatable. The original split was the right shape for that world, and
  the deleted workflows are recoverable from history. Restoring them means
  restoring the `workflow_run` alerts, so weigh a mechanical performance gate
  against seven suppressions per PR before doing it.
- **A second maintainer joins.** `prevent_self_review` on the `release`
  environment becomes usable, and the dispatch/approval split stops being
  nominal.
- **A dispatch is used to burn CI minutes.** The compute exposure above becomes
  real rather than theoretical; the response is to narrow who holds
  `Actions: write`, not to reintroduce an unreachable gate.
