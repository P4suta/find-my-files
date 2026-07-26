# ADR-0035: Automated versioning (release-please) + dev/nightly/stable build channels

Date: 2026-06-29 / Status: Accepted (supersedes the manual `xtask release` flow; extends ADR-0021's build-output layout with build *identity*)

ADR-0021 put every build artifact under one `build/` tree, but builds had no *identity*: `fmf --version` and the app's version label reported a bare `0.1.0` whether the binary was a contributor's local build, a future nightly, or an official release — all indistinguishable. And cutting a release meant a human ran `xtask release X.Y.Z`, hand-picking the number and hand-editing `engine/Cargo.toml` + the csproj in lockstep. That "version management is human-driven" shape is the thing to remove pre-v0.1, while builds are still free to change.

The decision criterion, set explicitly by the maintainer, is **convenience + how industry-standard/recommended a workflow is** — *not* daily build-loop cost (a small fluctuation there is acceptable). The dual-language reality (Rust `Cargo.toml` + C# `csproj`) is the practical filter: the tool must bump both.

## Decision

1. **release-please owns the version, CHANGELOG, tag, and draft release.** Conventional Commits on `main` drive a bot (`googleapis/release-please-action`, SHA-pinned) that keeps a "Release PR" open; merging it bumps the version, updates `CHANGELOG.md`, creates `vX.Y.Z` immediately (`"force-tag-creation": true`), and creates the GitHub Release as a **draft** (`"draft": true`). The forced tag is required so later Release PR calculations can find a draft release; GitHub otherwise delays the tag until publication. The maintainer never hand-picks or hand-edits a number — they merge a PR. The Release PR diff *is* the release preview (no local CLI needed). `release-please.yml` dispatches the exact tag to the dedicated performance gate; only its completed-success handler dispatches `release.yml`, which builds + signs, attaches assets to the draft, and publishes it (assets before publish, the order [immutable releases](https://docs.github.com/code-security/concepts/supply-chain-security/immutable-releases) demand). `release-please-config.json` + `.release-please-manifest.json` are the config.

2. **Version stays declared in the files; the bot edits them (not git-derived).** The package is the **repo root (`.`)** with `release-type: "simple"` and `extra-files`: a `toml` updater sets `engine/Cargo.toml` `$.workspace.package.version`, and a `generic` updater (keyed on an `x-release-please-version` annotation) sets the csproj `<Version>`. We do **not** use release-please's `rust` release-type: it can't write a workspace-*inherited* version (`version.workspace = true`) and fails with "value at path package.version is not tagged" (googleapis/release-please#2478, #1170). Because the `toml` updater bumps `Cargo.toml` but not `engine/Cargo.lock` (and CI is `--locked`), `release-please.yml` runs `cargo update --workspace` on the Release PR branch to sync the lock (no compile; re-runs on every PR rebuild so it self-heals). The package **must** be the repo root, not `engine`: `extra-files` paths are resolved relative to the package dir and **cannot use `..`**, so reaching both `engine/Cargo.toml` and `app/.../FindMyFiles.csproj` requires the package to sit above both — which also lets `CHANGELOG.md` live at the repo root. The manifest keeps the version present and reproducible (tarball/`.git`-less builds, `cargo metadata`, debuggability) — the "shackle" was the *human driver*, already removed by (1), not the stored number.

3. **Three channels, stamped at build time.** A new leaf crate `fmf-buildstamp` (depended on only by `fmf` + `fmf-service`, never `fmf-core`/`fmf-ffi`) resolves `VERSION` in `build.rs`; the C# csproj computes `InformationalVersion`. The base `X.Y.Z` is the release-please-managed number; the channel suffix is layered at build time:
   - **dev** (local `just build`) → `X.Y.Z-dev+g<sha>` (`.dirty` when the tree is dirty)
   - **nightly** → `X.Y.Z-nightly.<date>+g<sha>`
   - **stable** → clean `X.Y.Z`
   `xtask version --channel <dev|nightly|stable> [--date]` is the single source of the string *format*; CI exports it as `FMF_BUILD_VERSION` (Rust) / `FmfChannel` (C#).

4. **Conventional Commits are enforced.** Locally via a lefthook `commit-msg` hook (`committed`, mise-pinned); on PRs via the existing `amannn/action-semantic-pull-request` title gate (squash-merge → the PR title becomes the commit, so the title is what release-please reads).

5. **Nightly = unsigned, 14-day GitHub Actions artifact — not a Release.** `nightly.yml` builds the bundle from `main` (skipping when `main` is unchanged in 24h), stamps it nightly, and uploads `find-my-files-nightly-<date>`. Artifacts keep nightlies off the Releases list (no confusion with stable) and sidestep **Immutable Releases** (no rolling tag to overwrite). Nightlies are deliberately unsigned; the approval-gated signing pipeline (ADR-0029) is stable-only.

6. **GitHub App credentials are fail-closed.** `release-please.yml` mints a short-lived, repo-scoped installation token with explicit contents/issues/pull-request permissions and hands it to release-please. Those secrets live in a dedicated **`release-please` environment** with a `main`-only deployment policy and no required reviewers; an ordinary bot run fails visibly if either credential is absent. Release mutation and workflow dispatch are separate jobs. The first API-only job validates tag/draft/target and protected-`main` lineage, then dispatches only the secretless performance workflow at the created tag. The elevated self-hosted runner has Contents-read only. After GitHub records that workflow as successful, a separate hosted `workflow_run` job that never checks out repository code validates the exact evidence artifact and alone receives Actions-write to dispatch `release.yml`. Recovery of an already-created draft deliberately needs no App token.

7. **A real release requires multiple deliberate, independent actions — defence in depth so an ambiguous instruction can't ship one.** Opening the Release PR does nothing. Cutting a release takes, in order: (a) adding `release: approved`; (b) merging the Release PR; (c) passing the exact-tag performance gate; (d) approving `sign`; and (e) separately approving `publish`. The approval check is the independent required workflow `release-gate.yml`; it recognizes a Release PR from its manifest diff/bot branch as well as the mutable pending label, invalidates a surviving approval whenever the head changes, and keeps label events away from `ci-required`. `release.yml` is dispatch-only, executes from the exact created tag, and revalidates performance evidence plus workflow SHA = tag = draft target before build, signing, attestations, and publication. A stray tag starts nothing. The agent never merges the Release PR, pushes a version tag, approves an environment, or runs `publish=true` without an explicit version-named instruction.

## Rationale

- **release-please over in-tree (git-cliff + xtask) [the earlier lean]**: once daily-loop cost is *not* a criterion, a bespoke in-tree release script is neither the most convenient nor the most standard option — it is a maintained reinvention. The Release-PR bot is the lower-friction, more-recommended 2024+ workflow and is what the maintainer chose. The reversal is deliberate, recorded here, not drift.
- **release-please over release-plz**: release-plz is the Rust-native gold standard but only bumps Cargo crates; the C# csproj would be a bolt-on. release-please's `generic`/`extra-files` updater bumps *both* languages from one config — the decisive factor for a dual-language repo.
- **Declared-and-bot-edited over git-derived (nbgv/vergen)**: a height/`git describe` version is not Conventional-Commits-*semantic* (it can't turn `feat:` into a minor bump), breaks on `.git`-less source builds, and needs two separate tools for two languages. The stored number costs almost nothing and keeps reproducibility/debuggability.
- **Channel suffix at build time, base in the file**: the stamp (`fmf-buildstamp` / `InformationalVersion`) is the right home for the *derived* part (channel + sha); the *declared* base never needs git at build time. `fmf-buildstamp` is a leaf off the two front-end binaries so the `.git/HEAD` rerun never rebuilds the hot engine crates.
- **Artifacts for nightly**: with Immutable Releases on, a rolling `nightly` tag can't overwrite assets; dated prereleases would accumulate and need GC. A 14-day artifact auto-expires and is the least-moving-parts "separate bucket".

## Rejected alternatives

- **In-tree git-cliff + xtask (self-authored release command)** — maximum control and reuses the existing `xtask` version-edit code, but non-standard and a maintenance burden; loses on the chosen "convenience + recommended" axis. Rejected (was the prior lean; consciously overturned).
- **release-plz (Rust-native bot)** — idiomatic for the engine, but Rust-only: the C# version would need a second mechanism. Rejected for a dual-language repo.
- **nbgv + vergen (git-derived, no stored version)** — the purest "no version to manage" model, but non-composable with CC-semantic bumps, `.git`-dependent, and two-tool. Rejected.
- **Keep manual `xtask release X.Y.Z`** — simplest diff, but it *is* the human-driven shackle this ADR removes. Rejected.
- **Nightly as dated GitHub pre-releases** — publicly downloadable without a login, but accumulates under Immutable Releases and needs a GC workflow. Deferred behind a trigger.
- **Nightly as a rolling `nightly` Release** — the common "always-latest" pattern, but **incompatible with Immutable Releases** (can't overwrite the asset). Rejected outright.
- **CalVer (`YYYY.MM.x`)** — used by some apps, but a filename-search engine/CLI benefits from SemVer's change-magnitude signal, and CC→SemVer is the mainstream pairing. Rejected.

## Consequences

- The maintainer's release ritual becomes "write Conventional Commits, merge the Release PR." `just release` and the `xtask` version-edit modules (`release.rs`, `version/{cargo_toml,csproj}.rs`) are **removed** — versioning has one owner (release-please), not two.
- `release.yml` is dispatch-only and a real publication must run at the exact Release Please tag; it stamps the stable build cleanly (`FMF_BUILD_VERSION` / `FmfChannel=stable`).
- Release automation is split across the version bot, exact-tag performance gate,
  hosted completion dispatcher, and release workflow. No job both executes
  self-hosted repository code and holds publication authority.
- Every contributor commit must be a Conventional Commit (local hook + PR-title gate). `--no-verify` remains forbidden.
- The wire contract and golden corpus are untouched — the version string is not part of the wire format, so no golden re-capture.
- **First activation must be verified**: the first Release PR should show `engine/Cargo.toml` `[workspace.package] version` bumped (by the `toml` updater), `engine/Cargo.lock` synced (by the `cargo update --workspace` step, as a follow-up commit on the PR branch), the csproj `<Version>` bumped, and `CHANGELOG.md` written. Three real gotchas were hit and fixed during bring-up: release-please's `rust` release-type **cannot** write workspace-inherited versions (→ `simple` + `toml` updater + the lock-sync step); `extra-files` paths are **package-relative and reject `..`** (→ the package is the repo root so it can reach both `engine/` and `app/`); and `changelog-path` likewise rejects `..`.

## Re-examination triggers

- **Anonymous public nightly downloads wanted** → promote nightly artifacts to dated GitHub pre-releases + a retention/GC workflow.
- **Signed nightly wanted** → add a `sign` job to `nightly.yml` (reusing ADR-0029's pipeline). Per [ADR-0040](0040-nightly-supply-chain-parity.md), nightly now carries the rest of the supply chain (CycloneDX SBOMs, the osv-scanner gate, and keyless build-provenance + SBOM attestations); **signing is the only remaining stable-only supply-chain gate**, so this trigger is the sole nightly/release difference left.
- **release-please's Cargo-workspace handling proves insufficient** (version or `Cargo.lock` not bumped correctly) → switch the Rust side to a `toml` extra-file updater + an explicit `cargo update -p` lock-refresh, or adopt the `cargo-workspace` plugin.
- **crates.io / NuGet publishing begins** → re-evaluate release-plz (Rust) and a real package-publish step; the current config publishes nothing to a registry.
- **The C# csproj surface grows complex** (multiple version-bearing props) → reconsider Nerdbank.GitVersioning for the .NET side specifically.
