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
The publication job reads the two synchronized App fields from the protected `release-please` environment, creates attestations, and publishes the complete draft.
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
Preflight and both environment deployment policies independently reject another ref.
The production workflow has no rehearsal entry point.
Never replace published assets or rewrite a published tag to recover a failed attempt.

## Nightly

`nightly.yml` publishes an unsigned Actions artifact from main with fourteen-day retention and the `X.Y.Z-nightly.<date>+g<sha>` identity.
Nightly receives the same SBOM scan and GitHub attestations as stable.
It produces no Authenticode signature or GitHub release.

## Repository and credential setup

Apply the checked-in rulesets and enable immutable releases.
Status checks and conversation resolution remain mandatory.
Approving, code-owner, and last-push reviews remain off for the solo-maintainer configuration.
Enable all three review gates only after a distinct maintainer joins CODEOWNERS and can provide independent approval.

Reuse the shared release-please App with Administration read, Contents write, Issues write, and Pull requests write permissions for `find-my-files`.
Keep release-please restricted to protected main with administrator bypass off.
Keep release restricted to protected main with required reviewers and administrator bypass off.
The separate signing and publication approvals remain mandatory.

Store the App client ID and its downloaded private key in shared-automation.release_please as GITHUB_APP_CLIENT_ID and GITHUB_APP_PRIVATE_KEY.
The `find-my-files.automation` config references those canonical fields.
The [sync manifest](../automation/release-app-sync.toml) exposes only RELEASE_APP_CLIENT_ID and RELEASE_APP_PRIVATE_KEY through the dedicated github_automation config, alongside Doppler's generated metadata.
Use [native GitHub synchronization](https://docs.doppler.com/docs/github-actions) to deliver the fields to release-please.
The shared App key retains authority across installations.
Each pinned token action requests only the consuming repository and its explicit permissions.

Windows signing retains the read-only service account for `find-my-files.sign_windows` without a workplace role.
Its SSLDOTCOM_USERNAME, SSLDOTCOM_PASSWORD, SSLDOTCOM_CREDENTIAL_ID, and SSLDOTCOM_TOTP_SECRET fields reference shared-signing.windows.
Keep private key line breaks intact and use the persistent signing TOTP seed.
The owner maintains values directly in Doppler without sending them through chat or the repository.
Inspect names and access metadata only.
GitHub can't return stored secret values.

The signing identity retains audience `https://github.com/P4suta`, repository ID `1267203446`, owner ID `42543015`, and ref `refs/heads/main`.
Its subject is `repo:P4suta/find-my-files:environment:release`.
Restrict access to workflow_dispatch from release.yml or verify-doppler.yml at `refs/heads/main`.
Keep its public ID in DOPPLER_SIGN_WINDOWS_IDENTITY_ID.
Check the repository's OIDC subject customization before provisioning a replacement identity.
Publication retains GitHub OIDC for Sigstore attestations.

Run `mise x -- just lint-actions` before publishing workflow changes.
After the reviewed cutover reaches main, run the read-only App verification:

```powershell
gh workflow run verify-doppler.yml --repo P4suta/find-my-files --ref main
```

The default invocation skips signing verification.
Selecting verify_signing also checks signing credential delivery and unrelated-config denial behind the existing release approval.
Verification performs no signing, tagging, release creation, or publication.
Keep the existing App identity and source config for a reviewed revert.
See [credential delivery](credential-delivery.md) for synchronization, token revocation, and recovery.

## Performance baselines

Record baselines on the idle reference machine from a cold start.
Use `just bench-baseline` for the real-volume baseline and `just bench-micro-baseline` for Criterion.
The real-volume baseline lands through an ordinary reviewed PR, while the Criterion baseline remains machine-local.
Capture both baselines through their maintained commands.

Design rationale remains in ADR-0013, ADR-0020, ADR-0029, ADR-0034, ADR-0035, ADR-0038, ADR-0040, and ADR-0048.
