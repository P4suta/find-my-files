# Releasing

The workflows are the executable release specification.
The release guide records the human decisions that remain outside automation.

## Stable release

Land Conventional Commits on main.
Release Please maintains the version, lockfile, changelog, and draft Release PR.
Require green CI and run `just ui-test` without elevation on the final candidate.

On a clean standard-user account, accept the real User Account Control (UAC) service-install prompt and confirm that the same app process and window become searchable without relaunching.
Automation can't drive the secure desktop.

Run `just perf-gate` on the idle reference machine from a cold start.
Complete the performance gate before applying release approval or approving signing and publication.
A regression stops the release.
The measurement instrument requires an organization runner group that a user-owned repository can't create, so ADR-0048 keeps the performance gate local.

Add `release: approved` to the final Release PR head and merge the PR.
A later head update requires removing and reapplying the label.
Never edit the version or create a v* tag manually.
Release Please creates the draft and tag, then dispatches release.yml from protected main with the exact tag, commit, and numeric draft release ID.

Approve sign and then the secretless publish-approval job in the protected release environment.
Confirm the expected `vX.Y.Z` and immutable SHA each time.
The API-only publication job reads the two synchronized App fields from release-please.

Preflight binds the approved tag, source SHA, and draft release ID before any later job starts.
Every later job revalidates the same identities, and a stray tag can't publish.
Keep individual downstream jobs on their existing admission path.

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

The release must contain the ZIP, SHA256SUMS.txt, and both CycloneDX SBOMs.
The archive requires the exact release identity.
The SBOM attestations cover the Rust engine and the .NET app.

If automatic dispatch fails after creating a draft, dispatch release-please.yml from main with the existing `vX.Y.Z` tag.
The dispatcher validates the tag, draft, and target, fixes the numeric draft ID, and skips dispatch when a run already owns the same tuple.
If release-please is unavailable, an authorized release can use the direct recovery command with the same three values:

```powershell
gh workflow run release.yml --repo P4suta/find-my-files --ref main `
  -f tag_name=vX.Y.Z -f commit_sha=<40-hex tag commit> -f release_id=<numeric draft ID>
```

The main ref selects the trusted workflow code.
Preflight and both environment deployment policies independently reject another ref.
The production workflow has no rehearsal entry point.

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
