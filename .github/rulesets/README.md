# Repository rulesets (source of truth)

These JSON files are the version-controlled definition of this repo's branch
[rulesets](https://docs.github.com/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets).
`main`'s protection lives entirely in rulesets — there is **no** classic branch
protection (migrated 2026-06-28).

| File | Target | Enforces |
|---|---|---|
| `protect-default-branch.json` | `refs/heads/main` | PR path (solo-maintainer-safe: zero required approvals), `ci-required` + independent `release-gate` + C#/Actions CodeQL + RustSec `audit` checks (strict), linear history, conversation resolution, no force-push, no deletion, no admin bypass |
| `protect-version-tags.json` | `refs/tags/v*.*.*` | creation allowed for Release Please; existing version tags cannot be moved or deleted |
| `require-signed-commits.json` | all branches except `gh-pages` | signed commits |

> GitHub does not auto-apply repository rulesets from the tree. These files are
> the reviewable disaster-recovery templates; after a UI/API change, re-export
> the live ruleset here. `just rulesets-check` (below) is what proves the
> re-export actually happened.
> The checked-in default-branch template deliberately sets approving reviews to
> zero and disables code-owner/last-push approval: with only one maintainer in
> `CODEOWNERS`, enabling those gates would deadlock that maintainer's own PRs.
> Re-enable one approving review, code-owner review, and last-push approval only
> after a distinct maintainer has been added to `CODEOWNERS` and can actually
> supply the independent review; then re-export the live ruleset.
>
> `release-gate` must remain a separate required context. Folding label events
> into CI allows a label-only run to replace a failed `ci-required` result with a
> skipped/green result for the same commit. It identifies Release PRs from the
> manifest diff and bot branch as well as the mutable pending label, and a head
> update invalidates any surviving approval until the label is re-applied.
> Unrelated label events deliberately publish a different check context.

Re-export after a settings change (strips volatile fields):

```sh
gh api repos/P4suta/find-my-files/rulesets/<id> \
  --jq 'del(.id,.node_id,.created_at,.updated_at,._links,.current_user_can_bypass,.source,.source_type)'
```

## Detecting drift

A hand re-export that nobody performs leaves no trace: the tree keeps describing
protection the repository does not have, and the repository keeps enforcing
rules no reviewed file describes. `just rulesets-check` compares the two:

```sh
just rulesets-check
```

It captures every live ruleset into `build/rulesets/live/<id>.json` with
read-only `gh api` GETs, matches the files here against them **by `name`**, and
exits non-zero on any difference. It reports three kinds of finding:

- **NOT APPLIED** — a template here with no live ruleset of that name, so the
  repository does not enforce it at all.
- **UNTRACKED** — a live ruleset no template describes, which a
  disaster-recovery restore from this tree would silently drop.
- **DRIFTED** — the same name on both sides with different content, reported per
  rule type (`rules[pull_request].parameters.…`) because `rules` is an unordered
  array. Required status checks are compared as a set of contexts, so a check
  that no longer blocks a merge is named outright.

The comparison ignores only the fields the API assigns for its own bookkeeping —
`id`, `node_id`, `created_at`, `updated_at`, `_links`, `source`, `source_type`,
`current_user_can_bypass` — the same list the re-export snippet above strips.
`bypass_actors` is **not** ignored: who may bypass a ruleset is part of the
protection. A field that appears only on the live side (GitHub adds new rule
parameters over time) is reported as drift, not swallowed — it means the
template here predates the field and needs a re-export.

The command never writes a ruleset. Applying a template, and deciding whether a
difference is fixed by re-exporting the live ruleset or by changing the live
settings, are maintainer actions.

**Why this is not a CI job.** The rulesets endpoint answers
`X-Accepted-OAuth-Scopes: repo` — the whole-repository scope, with no narrower
one accepted. A workflow's `GITHUB_TOKEN` is scoped instead by the `permissions:`
block, whose keys are a closed per-resource set (`contents`, `checks`, `issues`,
`pull-requests`, …) with nothing for repository settings; `actionlint` rejects
`administration:` outright as an unknown permission scope. So a workflow could
only read rulesets through a long-lived admin PAT — a credential with far more
power than this check needs, stored where every workflow run can reach it. The
check runs locally instead, under the maintainer's own `gh` login, in the same
session that changes the settings.
