# Branch rulesets (source of truth)

These JSON files are the version-controlled definition of this repo's branch
[rulesets](https://docs.github.com/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets).
`main`'s protection lives entirely in rulesets — there is **no** classic branch
protection (migrated 2026-06-28).

| File | Target | Enforces |
|---|---|---|
| `protect-default-branch.json` | `refs/heads/main` | PR required, `ci-required` + `analyze` status checks (strict), linear history, conversation resolution, no force-push, no deletion, no admin bypass |
| `require-signed-commits.json` | all branches except `gh-pages` | signed commits |
| `protect-version-tags.json` | `refs/tags/v*` | `v*` tag creation/deletion allowed only by the release-please App (`bypass_actors` `actor_id: 4169572`, an Integration) — no stray/manual/agent tag push can start `release.yml` (ADR-0035) |

> GitHub does **not** auto-apply repository rulesets from files in the tree
> (only org-level rulesets can be imported). These are the canonical record and
> a disaster-recovery template; apply/verify them with `gh` per the runbook in
> [`docs/SCORECARD.md`](../../docs/SCORECARD.md#branch-protection--rulesets-runbook).
> When you change a ruleset in the GitHub UI or via `gh`, re-export it here so
> the tree stays the source of truth.

Re-export after a settings change (strips volatile fields):

```sh
gh api repos/P4suta/find-my-files/rulesets/<id> \
  --jq 'del(.id,.node_id,.created_at,.updated_at,._links,.current_user_can_bypass,.source,.source_type)'
```
