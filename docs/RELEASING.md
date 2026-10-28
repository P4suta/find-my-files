# Releasing

The workflows are the executable release specification. This page contains only
the human decisions that cannot be encoded safely.

## Stable release

1. Land Conventional Commits on `main`. Release Please maintains the version,
   lockfile, changelog, and draft Release PR.
2. Confirm CI is green and run `just ui-test` unelevated.
3. On a clean standard-user account, perform the one secure-desktop check
   automation cannot drive: accept the real UAC service-install prompt and
   confirm the same app process/window becomes searchable without relaunching.
4. Add `release: approved` to the final Release PR head and merge it (a later
   head update requires removing and re-adding the label). Never edit the
   version or create a `v*` tag manually. Release Please creates the draft/tag,
   and `release-please.yml` immediately dispatches `release.yml` from protected
   `main` with that exact tag, commit, and draft Release ID.
5. Run `just perf-gate` on the reference machine, cold and idle, before
   approving anything. CI cannot do this (ADR-0048): the measurement instrument
   requires an organization runner group that a user-owned repository cannot
   create, so this is the performance gate. A regression here stops the release.
6. Approve `sign` and then the secretless `publish-approval` job in the protected
   `release` environment. Confirm the expected `vX.Y.Z` and immutable SHA each
   time. The subsequent API-only publication job obtains App credentials from
   `release-please`.

The workflow is the executable specification: `preflight` binds the approved
tag, source SHA, and draft Release ID before anything else runs, every later job
revalidates the same identities, and a stray tag cannot publish. Do not bypass
or replay individual downstream jobs.

## Verify the published artifact

```powershell
$sum = (Get-Content -LiteralPath SHA256SUMS.txt) -split '\s+', 2
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $sum[1]).Hash -ne $sum[0]) { throw "checksum mismatch" }
$firstParty = @(
  "FindMyFiles.exe", "app\FindMyFiles.exe", "app\FindMyFiles.dll",
  "app\fmf-service.exe", "app\fmf_engine.dll"
)
foreach ($path in $firstParty) { signtool verify /pa /tw /v $path }
gh attestation verify find-my-files-vX.Y.Z-win-x64.zip --repo P4suta/find-my-files
```

The Release must contain the zip, `SHA256SUMS.txt`, and both CycloneDX SBOMs.
The ZIP must have the exact release identity plus Rust and .NET SBOM
attestations.

If automatic dispatch fails after a draft was created, dispatch
`release-please.yml` from `main` with that existing `vX.Y.Z` tag. It validates
the tag/draft/target, fixes the draft's numeric ID once, and re-dispatches
`release.yml` for that exact tag commit and Release ID — skipping the dispatch
if a run already owns that triple. If release-please itself is the thing that is
broken, dispatch the release directly with the same three values:

```powershell
gh workflow run release.yml --repo P4suta/find-my-files --ref main `
  -f tag_name=vX.Y.Z -f commit_sha=<40-hex tag commit> -f release_id=<numeric draft ID>
```

`--ref main` is not optional: `workflow_dispatch` loads workflow YAML from the
selected ref. Any other ref is refused by `preflight` and, independently, by the
protected-`main` deployment policy on the `release` and `release-please`
environments.

## Nightly

`nightly.yml` publishes a 14-day unsigned Actions artifact from `main`, stamped
`X.Y.Z-nightly.<date>+g<sha>`. It receives the same SBOM scan and GitHub
attestations as stable, but no Authenticode signature and no GitHub Release.

One-time repository setup still matters: apply `.github/rulesets/` and enable
immutable Releases. The checked-in default-branch ruleset is solo-maintainer-safe:
status checks and conversation resolution remain mandatory, but approving,
code-owner, and last-push reviews are disabled. Re-enable all three review gates
only after a distinct maintainer has been added to `CODEOWNERS` and can provide
the independent approval.

Use the existing shared release-please GitHub App, installed for this repository with Administration:read, Contents:write, Issues:write, and Pull requests:write.
The `release-please` GitHub environment selects the App-only Doppler identity and remains restricted to protected `main`.
Keep the `release` environment restricted to protected `main`, with required reviewers and admin bypass disabled; it selects the signing-only Doppler identity.

## Doppler setup

Create the following configs and read-only Service Accounts, with no workplace role:

| Purpose | Project | Config | Service Account | GitHub environment |
|---|---|---|---|---|
| Release automation and publication | `find-my-files` | `automation` | `find-my-files-automation` | `release-please` |
| Windows signing | `find-my-files` | `windows` | `find-my-files-windows` | `release` |

The automation config exposes `RELEASE_PLEASE_CLIENT_ID` and `RELEASE_PLEASE_PRIVATE_KEY` through references to the shared release-please App.
The Windows config exposes only `ES_USERNAME`, `ES_PASSWORD`, `CREDENTIAL_ID`, and `ES_TOTP_SECRET`.
Prepare missing input fields as empty strings with explanatory notes, and configure the references below before handoff.
The owner populates existing values directly in Doppler from their protected credential store.
Agents inspect secret names and access metadata only; they do not access 1Password.
GitHub cannot return stored secret values.
Keep PEM private key line breaks intact and use the persistent TOTP seed for `SSLDOTCOM_TOTP_SECRET`.
Fill and save the four prepared fields in [shared-signing / windows](https://dashboard.doppler.com/workplace/projects/shared-signing/configs/windows) and the two prepared fields in [shared-automation / release_please](https://dashboard.doppler.com/workplace/projects/shared-automation/configs/release_please).

Store the existing shared release-please GitHub App's Client ID and PEM private key once in `shared-automation.release_please`, under `GITHUB_APP_CLIENT_ID` and `GITHUB_APP_PRIVATE_KEY`.
Store the distinct release-plz App's credentials separately in `shared-automation.release_plz` for repositories using that App.
Each repository keeps its own release-please or release-plz workflows, GitHub environments, and purpose-scoped Doppler access.
Mint installation tokens for the consuming repository with only the permissions required by each step.
The shared App private key retains authority across the App's installations; repository-scoped installation tokens limit the normal consuming operations.
Use these references in `find-my-files.automation`:

| Consuming name | Reference |
|---|---|
| `RELEASE_PLEASE_CLIENT_ID` | `${shared-automation.release_please.GITHUB_APP_CLIENT_ID}` |
| `RELEASE_PLEASE_PRIVATE_KEY` | `${shared-automation.release_please.GITHUB_APP_PRIVATE_KEY}` |

For intentionally shared Windows credentials, populate `shared-signing.windows` and use these references in `find-my-files.windows`:

| Consuming name | Reference |
|---|---|
| `ES_USERNAME` | `${shared-signing.windows.SSLDOTCOM_USERNAME}` |
| `ES_PASSWORD` | `${shared-signing.windows.SSLDOTCOM_PASSWORD}` |
| `CREDENTIAL_ID` | `${shared-signing.windows.SSLDOTCOM_CREDENTIAL_ID}` |
| `ES_TOTP_SECRET` | `${shared-signing.windows.SSLDOTCOM_TOTP_SECRET}` |

Create one GitHub OIDC identity for each Service Account.
Require all of these common claims:

| Claim | Exact allowed value |
|---|---|
| `aud` | `https://github.com/P4suta` |
| `repository_id` | `1267203446` |
| `repository_owner_id` | `42543015` |
| `ref` | `refs/heads/main` |

The automation identity additionally requires `sub` = `repo:P4suta/find-my-files:environment:release-please`, `event_name` = `push` or `workflow_dispatch`, and `workflow_ref` = `P4suta/find-my-files/.github/workflows/release-please.yml@refs/heads/main`, `P4suta/find-my-files/.github/workflows/release.yml@refs/heads/main`, or `P4suta/find-my-files/.github/workflows/verify-doppler.yml@refs/heads/main`.
The signing identity additionally requires `sub` = `repo:P4suta/find-my-files:environment:release`, `event_name` = `workflow_dispatch`, and `workflow_ref` = `P4suta/find-my-files/.github/workflows/release.yml@refs/heads/main` or `P4suta/find-my-files/.github/workflows/verify-doppler.yml@refs/heads/main`.
The repository ID prevents a same-named replacement repository from inheriting access.
Check `gh api repos/P4suta/find-my-files/actions/oidc/customization/sub` before provisioning: these subjects use the existing name-based prefix, and must be updated if the repository later enables immutable subjects.

Set each identity's public ID as `DOPPLER_SERVICE_IDENTITY_ID` in its corresponding GitHub environment variable.
Keep build, SBOM, mutation, collection, packaging, and publication approval jobs without Doppler access.
A failed fetch, empty output, or unresolved reference stops the credential-consuming job before signing or minting an App token.

Validate workflow syntax and security with `mise x -- just lint-actions`.
Run `gh workflow run verify-doppler.yml --repo P4suta/find-my-files --ref main` to verify allowed and denied CI access before a real release.
The manual verification uses the existing environments, including the signing environment's required approval, and checks resolved credentials without printing values.
It verifies the App with a read-only repository token and requires each Doppler identity to be denied access to the other purpose's config.
It does not sign binaries, create tags or releases, or dispatch the production release workflow.
The production release workflow has no rehearsal entry point.
Remove the old GitHub environment secrets only after their Doppler paths work and the original credentials remain recoverable in the approved store.

[OIDC identities](https://docs.doppler.com/docs/service-account-identities), [the official fetch action](https://github.com/DopplerHQ/secrets-fetch-action), and [secret references](https://docs.doppler.com/docs/secrets) describe the provider interfaces.

## Performance baselines

Performance baselines are recorded by hand on the reference machine, cold and
idle: `just bench-baseline` for the real-volume baseline and
`just bench-micro-baseline` for the Criterion suite. The real-volume result
(`engine/benches/baseline.json`) lands through an ordinary reviewed PR; the
Criterion baseline is machine-local. Never hand-edit or fabricate either.

Design rationale lives in ADR-0013, ADR-0020, ADR-0029, ADR-0034, ADR-0035,
ADR-0038, ADR-0040, and ADR-0048.
