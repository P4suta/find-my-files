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
> the live ruleset here.
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
