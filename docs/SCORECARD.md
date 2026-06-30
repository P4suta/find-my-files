# OpenSSF Scorecard

This repo publishes an [OpenSSF Scorecard](https://scorecard.dev/) report
(`.github/workflows/scorecard.yml`, badge in the README). Scorecard grades
supply-chain hygiene across ~18 checks. This page records **which checks we
act on in-repo, which need a one-time maintainer action, and which we
deliberately leave** — so the score is understood, not chased blindly.

Baseline when this page was written (2026-06-17): **5.9 / 10**.

## In-repo (already applied)

| Check | Before | What changed |
|---|---|---|
| **Token-Permissions** | 0 | `codeql.yml` / `pages.yml` / `release.yml` declared `write` scopes at the **top level**. Moved each to the single job that needs it; top level is now `contents: read`. Same effective token, least-privilege, and Scorecard rewards job-scoped writes. |
| **Fuzzing** | 0 | Added cargo-fuzz harnesses (`engine/fuzz/`) + a bounded Linux smoke (`fuzz.yml`). See *Fuzzing scope* below. |
| **Branch-Protection** | 3 | Added `.github/CODEOWNERS` (the file half of "require Code Owner reviews"). The settings half is the runbook below. `main`'s protection is now expressed as **rulesets** (`.github/rulesets/`), not classic branch protection. |

## Maintainer actions (cannot be done by committing files)

### Branch-Protection / rulesets runbook

The score (3) means protection exists but lacks approval/code-owner reviews.
The rules live in GitHub repo settings, not the tree — but the **definitions are
version-controlled** under `.github/rulesets/` as the source of truth. Apply with
`gh` (needs admin; run it yourself — we don't store admin tokens or write ad-hoc
scripts).

As of 2026-06-28 `main` is protected by **rulesets**, not classic branch
protection (classic was removed; rulesets layer cleanly, export to JSON, and are
GitHub's forward path). Three rulesets, all committed:

| File | Target | Enforces |
|---|---|---|
| `.github/rulesets/protect-default-branch.json` | `main` | PR required, `ci-required` + `analyze` checks (strict), linear history, conversation resolution, no force-push/deletion, no admin bypass |
| `.github/rulesets/require-signed-commits.json` | all branches but `gh-pages` | signed commits |
| `.github/rulesets/protect-version-tags.json` | `refs/tags/v*` | tag creation/deletion only by the release-please App (ADR-0035 release safety) |

GitHub does **not** auto-apply repo-level rulesets from the tree. Create or
update them from the committed JSON (existing ruleset → use `--method PUT
.../rulesets/<id>`; new → `POST`):

```sh
gh api --method POST repos/P4suta/find-my-files/rulesets \
  --input .github/rulesets/protect-default-branch.json
```

Verify the effective rules on `main` (this lists ruleset-sourced rules):

```sh
gh api repos/P4suta/find-my-files/rules/branches/main --jq '[.[].type] | unique'
# → deletion, non_fast_forward, pull_request, required_linear_history,
#   required_signatures, required_status_checks
```

After any UI/`gh` change, **re-export** the ruleset back into `.github/rulesets/`
(strip volatile fields) so the tree stays canonical — see
`.github/rulesets/README.md`.

There is a real tension: **requiring ≥1 approval conflicts with solo self-merge**
(you can't approve your own PR → every merge blocks). The committed config keeps
**Mode A** (self-merge, `required_approving_review_count: 0`). To switch to
**Mode B** (largest Scorecard bump; needs a second reviewer or you stop
self-merging), edit the `pull_request` rule in `protect-default-branch.json`:

```json
  { "type": "pull_request", "parameters": {
      "required_approving_review_count": 1,
      "require_code_owner_review": true,
      "dismiss_stale_reviews_on_push": true,
      "required_review_thread_resolution": true
  }}
```

then re-apply with `--method PUT .../rulesets/<id> --input <file>`.

> Confirm the status-check context names (`ci-required`, `analyze`) match the jobs
> in `ci.yml` / `codeql.yml` as GitHub reports them — check **Settings → Rules**
> if a context never resolves.

Honest ceiling: Scorecard's higher Branch-Protection tiers are gated on review
requirements, so a solo project realistically tops out in the mid range under
Mode A. Mode B is the only path to the top tier.

### CII / OpenSSF Best Practices — enrollment runbook

Self-attested, external ([bestpractices.dev](https://www.bestpractices.dev/)).
Scorecard only credits it once the badge reaches **passing** (in-progress = 0).

1. Register the project at <https://www.bestpractices.dev/en/projects/new>
   (repo URL `https://github.com/P4suta/find-my-files`).
2. Answer the *passing* questionnaire. This repo already satisfies the bulk of
   it — evidence map below.
3. Take the assigned project id `NNNN` and add the badge under the Scorecard
   badge in `README.md` (do this **after** enrollment so it never renders broken):

   ```markdown
   [![OpenSSF Best Practices](https://www.bestpractices.dev/projects/NNNN/badge)](https://www.bestpractices.dev/projects/NNNN)
   ```

Evidence map for the *passing* criteria (most are already met):

| Criterion | Evidence in this repo |
|---|---|
| Project homepage / description | `README.md`, GitHub Pages (`pages.yml`) |
| Version-controlled source, public | this Git repo |
| OSI license | `LICENSE` (Apache-2.0) |
| Contribution guide | `CONTRIBUTING.md` |
| Bug/issue reporting | `.github/ISSUE_TEMPLATE/`, Discussions |
| Vulnerability reporting process | `.github/SECURITY.md` (private advisories) |
| Build + automated tests | `just build` / `just test` / `just test-app`, enforced in `ci.yml` |
| Tests run on contributions (CI) | `ci.yml` on every PR |
| Static analysis | CodeQL (`codeql.yml`), Clippy, `cargo-audit` |
| Secured release / signing | `release.yml` (Authenticode + Sigstore attestations + SBOMs), `docs/SIGNING.md`, `docs/SUPPLY_CHAIN.md` |
| Unique versioning + release notes | SemVer tags, auto-generated release notes |
| HTTPS | GitHub + Pages are HTTPS-only |

The open items are typically a couple of "describe X" free-text answers, not
new engineering.

## Fuzzing scope

`fuzz.yml` runs `engine/fuzz/` on Linux because cargo-fuzz is effectively a
Linux/nightly tool. The harnesses cover both untrusted-input surfaces:

- The **named-pipe wire codec** (`fmf-proto` + `fmf-contract`) — the **privilege
  boundary** (non-elevated UI → elevated `fmf-service`, see `docs/SECURITY.md`).
  A hostile local client sending malformed frames to the elevated service hits
  these parsers first. Targets: `frame_decode`, `message_decode`.
- The **`fmf-core` decoders**: the query parser/compiler (`query_parse` — query
  text crosses the privilege boundary), the snapshot reader (`index_snapshot` —
  `unsafe` POD reads sized by an untrusted length prefix), the USN record parser
  (`usn_records`), and the WTF-8 codec (`wtf8_decode`). These became fuzzable by
  gating fmf-core's Windows deps (`ntfs-reader` / `windows-sys`) behind
  `[target.'cfg(windows)']` + `#[cfg(windows)]` on the `mft` / `scan` / `engine`
  modules, so the pure parsers compile for `x86_64-unknown-linux-gnu`. This is
  *not* cross-platform support (the won't-do list stands — the app stays
  Windows-only); it only lets the OS-independent parsers be instrumented.

Every surface keeps its in-tree `proptest` no-panic/round-trip coverage on
Windows too; libFuzzer adds coverage-guided exploration with ASan on top.

> ADR-0021 note: cargo-fuzz writes `corpus/`, `artifacts/`, `target/` next to
> the fuzz crate and its dir model doesn't compose with the `build/` redirect,
> so those are git-ignored in place (`.gitignore`). They're nightly, CI-only,
> machine-local.

## Deliberately left (not movable by repo changes)

| Check | Score | Why we leave it |
|---|---|---|
| Code-Review | 0 | Solo self-merge → 0 approved changesets. Needs a second reviewer (see Branch-Protection Mode B). |
| Contributors | 0 | Scores contributors' org affiliations; not meaningful for a solo personal repo. |
| Maintained | 0 | Repo is < 90 days old. Resolves with time + activity. |
| Signed-Releases | -1 | No release cut yet (inconclusive, excluded from the average). The infra (`release.yml`: Authenticode + attestations + SBOMs) is ready; the first tagged release should score well. |
| Packaging | -1 | Same — no published release to detect yet. |

Cutting the first release is a product decision, not a Scorecard chore, so it's
out of scope for this hardening pass.

## Re-checking the score

After merging, trigger a fresh scan (`scorecard.yml` runs weekly, on push to
`main`, and via **Actions → scorecard → Run workflow**), then read the badge or
<https://scorecard.dev/viewer/?uri=github.com/P4suta/find-my-files>.
