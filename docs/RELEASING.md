# Releasing

find-my-files versions itself from Conventional Commits — there is no manual
version bump. This page covers how a release happens, how to **activate** the
automation, the **nightly** channel, and the **build-identity** stamping.
Design rationale: [ADR-0035](adr/0035-automated-versioning-with-release-please-and-build-channels.md).

## How a release happens (once activated)

1. Conventional Commits land on `main` (squash-merged PRs; the PR title is the commit).
2. [`release-please`](../.github/workflows/release-please.yml) keeps a **Release PR** open
   that bumps `engine/Cargo.toml` (`[workspace.package] version`, then a `cargo update
   --workspace` step syncs `engine/Cargo.lock`) and `app/FindMyFiles/FindMyFiles.csproj`,
   and updates [`CHANGELOG.md`](../CHANGELOG.md). The version is derived from the commits:
   `feat:` → minor, `fix:`/`perf:` → patch, `!` / `BREAKING CHANGE:` → major.
3. **Add the `release: approved` label** to the Release PR. Until it's there the
   `release-gate` check fails the PR (so a release is never an accidental merge).
4. **Merge the Release PR.** release-please cuts the `vX.Y.Z` tag and a GitHub Release.
5. The tag fires [`release.yml`](../.github/workflows/release.yml): build → **sign (approve in
   the `release` environment)** → **publish (approve again)**, attaching the signed bundle +
   `SHA256SUMS.txt`.

You never hand-pick or hand-edit a version. The Release PR diff *is* the preview.

## Release safety (defence in depth)

Cutting a real, immutable release is deliberately gated by several independent steps
(ADR-0035), so an ambiguous instruction can't ship one by accident:

- **Label gate** — the Release PR can't merge until you add `release: approved` (`release-gate`).
- **Manual merge** — the Release PR is never auto-merged. The repo-wide auto-merge
  feature can't be hidden per-PR, so `no-automerge-on-release-pr.yml` turns it back
  off if it's ever armed on a release-please PR (normal PRs are unaffected).
- **Tag protection** — a ruleset allows `v*.*.*` tag creation only by the release-please App,
  so no stray/manual tag push can start the pipeline.
- **Two environment approvals** — both the `sign` and `publish` jobs pause on the `release`
  environment (reviewer = the maintainer); the irreversible publish has its own approval.
- **Agent contract** — automated tooling (incl. the AI assistant) will not merge the Release PR,
  push a `v*` tag, approve the `release` environment, or run `release.yml` with `publish=true`
  without an explicit, version-named instruction.

## Activation (one-time)

release-please ships **dormant**: with the App secrets unset, `release-please.yml` runs
green and no-ops. It runs as a **GitHub App** because a tag pushed by the default
`GITHUB_TOKEN` does **not** trigger `release.yml` (GitHub's workflow-recursion guard) — so
the tag must be pushed by a different identity. The workflow mints a short-lived
installation token at runtime via `actions/create-github-app-token`.

1. **Create a GitHub App** (org or personal). Repository permissions: **Contents:
   Read & write** and **Pull requests: Read & write**. No webhook needed.
2. **Install** the App on the `find-my-files` repo.
3. Generate a **private key** (`.pem`) for the App and note its **Client ID** (shown
   at the top of the App's **General** settings page, e.g. `Iv…`).
4. **Create an environment** for the credential (this is the hardened part — see below):
   Settings → Environments → **New environment** → name it **`release-please`**.
   - **Deployment branches and tags** → **Selected branches** → add **`main`** only.
   - **Do NOT add required reviewers** (release-please must run unattended).
5. In that environment's **Environment secrets**, add:
   - `RELEASE_PLEASE_CLIENT_ID` = the App's Client ID
   - `RELEASE_PLEASE_PRIVATE_KEY` = the full `.pem` contents (paste the whole file,
     `-----BEGIN…` through `…END-----`; multi-line is fine)

   (If an earlier setup used the old `RELEASE_PLEASE_APP_ID` /
   `RELEASE_PLEASE_APP_PRIVATE_KEY` secrets, delete them and re-add under the names
   above — the workflow now authenticates the App by **Client ID**, not App ID.)
6. (Optional, first run) To stop the first Release PR from scanning the entire history,
   set `bootstrap-sha` in `release-please-config.json` to a recent commit, or seed
   `.release-please-manifest.json` to the last shipped version.

### Why an environment, not a repository secret

A **repository** secret is readable by a workflow run on *any* branch (a push to a
feature branch included). The App private key carries `contents: write` +
`pull-requests: write`, so we scope it: the `release-please` job declares
`environment: release-please`, and the environment's branch policy (`main` only) means
only the main-branch release-please run can read the key — a workflow on another branch
is denied it. This mirrors how the signing secrets live in the `release` environment,
with one deliberate difference: **release-please's environment has no required reviewers**
(the human gate is merging the Release PR; the signing approval gate is separate).

> A fine-grained **PAT** with the same two permissions also works — drop the
> `create-github-app-token` step and pass the PAT directly as `token:`. The App is
> preferred (no human-tied credential; the token is short-lived and repo-scoped).

### The first release is pinned to 0.1.0 (one-time)

`release-please-config.json` sets `"release-as": "0.1.0"`. Without it, release-please
treats the first release from a `0.0.0` manifest as the **initial release → 1.0.0**,
which is wrong for a pre-1.0 project. `release-as` forces the first release to `0.1.0`.

> **After `v0.1.0` is released, delete the `"release-as": "0.1.0"` line** (it would
> otherwise pin every future release to 0.1.0). Once removed, the manifest is `0.1.0`
> and the next `feat:` correctly proposes `0.2.0`.

### Verify the first Release PR

The Cargo workspace uses inherited versions (`version.workspace = true`) and CI builds
`--locked`. On the **first** Release PR, confirm the diff bumps **all** of:

- `engine/Cargo.toml` `[workspace.package] version` (the `toml` extra-file updater)
- `engine/Cargo.lock` (the internal crates' versions — a follow-up `chore: sync Cargo.lock`
  commit from the `cargo update --workspace` step in `release-please.yml`)
- `app/FindMyFiles/FindMyFiles.csproj` `<Version>` (the `x-release-please-version` line)
- `CHANGELOG.md`

This `simple` + `toml` updater + lock-sync setup exists because release-please's `rust`
release-type can't write workspace-*inherited* versions (it fails with "value at path
package.version is not tagged"; googleapis/release-please#2478). If `engine/Cargo.lock`
shows stale (the Release PR's `--locked` CI would go red), the lock-sync step did not run —
check that `release-please.yml`'s sync step is gated on the Release PR existing.

## Nightly builds

[`nightly.yml`](../.github/workflows/nightly.yml) builds an **unsigned** bundle from the
tip of `main` daily (and on demand), stamped `X.Y.Z-nightly.<date>+g<sha>`. It is published
as a **14-day GitHub Actions artifact**, not a Release — keeping it off the Releases list and
clear of stable. Grab the latest:

```
gh run download --repo P4suta/find-my-files -n find-my-files-nightly
```

Nightlies are unsigned (SmartScreen will warn) and carry no stability guarantee. They are
login-gated and expire after 14 days; if anonymous public access is ever needed, see the
ADR-0035 trigger for promoting them to dated pre-releases.

## Build identity (channels)

`fmf --version`, `fmf-service --version`, and the app's F12 panel report a channel-aware
string so a build's origin is unambiguous:

| Channel | Example | Where |
|---|---|---|
| dev | `0.1.0-dev+g3672e3f` (`.dirty` if the tree is dirty) | local `just build` |
| nightly | `0.1.0-nightly.20260629+g3672e3f` | `nightly.yml` |
| stable | `0.1.0` | `release.yml` (tagged) |

The base `X.Y.Z` is the release-please-managed number; the channel suffix is layered at
build time. Rust uses the `fmf-buildstamp` crate (`build.rs`), driven by `FMF_BUILD_VERSION`;
C# uses the csproj `InformationalVersion`, driven by the `FmfChannel` MSBuild property. The
canonical string format comes from one place:

```
just version --channel nightly --date 20260629   # → 0.1.0-nightly.20260629+g<sha>
just version --channel stable                     # → 0.1.0
```
