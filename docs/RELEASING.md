# Releasing

The [release workflow](../.github/workflows/release.yml) is the executable release specification.
The operator owns source acceptance, native performance measurements, protected approvals, and the signed installation check.

## Stable release

Land reviewed Conventional Commits on protected `main`.
Release Please maintains the version, lockfile, changelog, and Release PR.
Keep the current head checks passing and run `just ui-test` without elevation.
An unsigned development ZIP supports packaging and search checks, but the [service elevation boundary](SECURITY.md) requires a trusted Authenticode signature.

Run `just perf-gate` on the reference machine while cold and idle before signing approval.
Keep the source identity and actual measurement reports with the acceptance evidence.
A performance regression stops the release.
Repeat invalidated measurements after a relevant source or configuration change.
Hosted CI doesn't supply the reference-machine measurement described in [ADR-0048](adr/0048-direct-dispatch-release-workflow.md).

Add `release: approved` to the accepted Release PR head and merge it.
A later head update requires removing and re-adding the label.
Release Please creates the version tag and draft Release.
Never edit the version or create a version tag manually.
The [automation workflow](../.github/workflows/release-please.yml) dispatches the release controller from protected `main` with the exact tag, commit, and numeric draft Release ID.

The controller binds that identity before building.
The secretless build, dependency scan, and full mutation campaign must pass before signing.
Approve `sign` in the protected `release` environment after checking the expected tag and immutable source commit.
The signing job receives the verified candidate without executing candidate-controlled hooks.

Wait for `package` to upload `release-assets` before approving `publish-approval`.
Download that artifact from the same run and check the ZIP checksum, signed bundle manifest, expected publisher, and timestamp.
The artifact contains the signed ZIP, `SHA256SUMS.txt`, both CycloneDX SBOMs, and the unsigned and signed bundle manifests.
Use those exact signed bytes for the remaining native acceptance checks.

On a clean standard-user account, accept the real User Account Control prompt for service installation.
Confirm that the existing app process and window become searchable without relaunching.
Verify version reporting, a useful search, upgrade from a supported installed version, and cleanup.
Record the Windows build, candidate digest, source commit, and result.
The secure desktop requires an operator.
UI automation doesn't supply that acceptance result.

Approve the secretless `publish-approval` job after the signed candidate passes those checks.
Check the same tag, source commit, and draft Release ID again.
The publication job obtains App credentials through `release-please`, creates attestations, and publishes the complete draft.
Every later stage revalidates the original identity.
A stray tag can't publish, and individual downstream jobs must not bypass the controller.

## Verify published bytes

Download the ZIP, `SHA256SUMS.txt`, and both CycloneDX SBOMs from the published Release.
Compare the ZIP checksum before extraction.
Verify every first-party signature in the extracted bundle, including the launcher, app executable and assembly, service, and engine library.
The Release identity and both SBOM attestations must bind the ZIP digest.

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

## Resume an existing draft

If automatic dispatch fails after draft creation, dispatch `release-please.yml` from `main` with the existing version tag.
The workflow validates the tag, draft, and target before dispatching the release controller.
An existing run that owns the same identity prevents duplicate dispatch.
If the automation workflow itself fails, the release controller accepts the same exact tag, commit, and numeric draft ID directly.

```powershell
gh workflow run release.yml --repo P4suta/find-my-files --ref main `
  -f tag_name=vX.Y.Z -f commit_sha=<40-hex tag commit> -f release_id=<numeric draft ID>
```

The dispatch must use `--ref main` because GitHub loads the workflow from the selected ref.
The controller and credential identities independently require protected `main`.
The production workflow has no rehearsal entry point.
Never replace published assets or rewrite a published tag to recover a failed attempt.

## Credential delivery

Use the existing shared release-please GitHub App with its installation scoped to the repository.
The App needs Administration:read, Contents:write, Issues:write, and Pull requests:write for the existing automation operations.
Each consuming step requests only its required installation-token permissions.
Keep the distinct release-plz App and its credentials separate.

The `release-please` GitHub environment uses `DOPPLER_AUTOMATION_IDENTITY_ID` to read `find-my-files.automation`.
The config exposes `GITHUB_APP_CLIENT_ID` and `GITHUB_APP_PRIVATE_KEY` through references to `shared-automation.release_please` under the same names.
The shared private key keeps authority across the App's installations.
Repository-scoped installation tokens limit the normal consuming operations.

The protected `release` environment uses `DOPPLER_SIGN_WINDOWS_IDENTITY_ID` to read `find-my-files.sign_windows`.
The config exposes `SSLDOTCOM_USERNAME`, `SSLDOTCOM_PASSWORD`, `SSLDOTCOM_CREDENTIAL_ID`, and `SSLDOTCOM_TOTP_SECRET` through references to `shared-signing.windows` under the same names.
The signing workflow maps those fields to the provider inputs and its local `ES_*` validation variables.
Keep the persistent TOTP seed in `SSLDOTCOM_TOTP_SECRET`.
Preserve the App private key's original serialization and line breaks.
The owner maintains credential values in the approved store.
Agents inspect names and access metadata.

Give each Doppler Service Account read-only access to its own config without a workplace role.
Use one OIDC identity per purpose.
Both identities require the following claims.

```json
{
  "aud": "https://github.com/P4suta",
  "repository_id": "1267203446",
  "repository_owner_id": "42543015",
  "ref": "refs/heads/main"
}
```

The automation identity requires `sub` equal to `repo:P4suta/find-my-files:environment:release-please`.
The allowed events are `push` and `workflow_dispatch`.
Its allowed `workflow_ref` values name `release-please.yml`, `release.yml`, or `verify-doppler.yml` under `P4suta/find-my-files/.github/workflows/` at `refs/heads/main`.

The signing identity requires `sub` equal to `repo:P4suta/find-my-files:environment:release` and `event_name` equal to `workflow_dispatch`.
Its allowed `workflow_ref` values name `release.yml` or `verify-doppler.yml` at the same protected path and ref.
The numeric repository ID prevents a same-named replacement repository from inheriting access.
Check `gh api repos/P4suta/find-my-files/actions/oidc/customization/sub` before provisioning.
Update the identity subjects if the repository enables immutable subjects.

Require owner reviewers and turn off administrator bypass for the protected signing and publication environment.
Keep automatic Release PR preparation free from a reviewer gate.
Require protected `main` in the controller and purpose-scoped OIDC claims.
Build, dependency scanning, mutation, collection, packaging, and publication approval need no Doppler access.
A failed fetch, empty field, or unresolved reference stops the consuming job before signing or minting an App token.
The [OIDC identity documentation](https://docs.doppler.com/docs/service-account-identities), [fetch action](https://github.com/DopplerHQ/secrets-fetch-action), and [reference documentation](https://docs.doppler.com/docs/secrets) define the provider interfaces.

Validate workflow syntax with `mise x -- just lint-actions`.
Before a real release, dispatch the separate read-only credential verification workflow from protected `main`.

```powershell
gh workflow run verify-doppler.yml --repo P4suta/find-my-files --ref main
```

The [verification workflow](../.github/workflows/verify-doppler.yml) uses the existing protected environments and requires their approvals.
It resolves required credentials without printing values, verifies a read-only repository App token, and requires cross-purpose config access to fail.
It doesn't sign, tag, publish, or invoke the production release controller.
Remove obsolete GitHub environment secrets after verification passes and the approved store still holds recoverable credentials.

## Nightly and repository protections

The [nightly workflow](../.github/workflows/nightly.yml) publishes a 14-day unsigned Actions artifact from `main`.
Its version has the form `X.Y.Z-nightly.<date>+g<sha>`.
It receives the same dependency scan and attestations as stable.
It has no Authenticode signature or GitHub Release.

Apply `.github/rulesets/` and enable Immutable Releases.
The solo-maintainer ruleset requires passing status checks and resolved conversations.
Enable independent approving, code-owner, and last-push reviews after a distinct maintainer joins `CODEOWNERS`.

## Performance baselines

Record performance baselines on the reference machine while cold and idle.
Use `just bench-baseline` for the real-volume baseline and `just bench-micro-baseline` for Criterion.
The real-volume result in `engine/benches/baseline.json` follows an ordinary reviewed PR.
The Criterion baseline stays machine-local.
Never fabricate or hand-edit either measurement.

The design records are ADR-0013, ADR-0020, ADR-0029, ADR-0034, ADR-0035, ADR-0038, ADR-0040, and ADR-0048.
