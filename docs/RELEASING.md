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
   version or create a `v*` tag manually. Release Please creates the draft/tag
   and automatically measures that immutable tag on the dedicated elevated
   runner.
5. Approve the `sign` and then `publish` jobs in the protected `release`
   environment. Confirm the deployment ref is the expected `vX.Y.Z` tag before
   each approval.

`release-please.yml` dispatches `performance-gate.yml` at the created tag.
After GitHub records the whole gate as successful, the hosted no-checkout
`performance-release.yml` job validates its evidence and dispatches
`release.yml`. The elevated measurement runner never receives publish
authority. Every boundary rechecks workflow SHA = tag = draft target.
`release.yml` builds, scans both SBOMs, Authenticode-signs every first-party PE,
verifies signer/chain/timestamp, packages checksums, creates GitHub provenance
and SBOM attestations, verifies immutable Releases are enabled, attaches all
assets, then publishes the draft. A stray tag does not trigger publishing.

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
The ZIP must have provenance plus Rust and .NET SBOM attestations.

## Signing-only rehearsal

Dispatch `release.yml` from `main` with `tag_name=main` and `publish=false`.
The bundle is stamped as a development build and no Release or attestation is
created. With all four signing secrets configured it must pass signature
verification; without them the run is explicitly a build-only rehearsal and
warns that the artifact is unsigned.

If automatic dispatch fails after a draft was created, dispatch
`release-please.yml` from `main` with that existing `vX.Y.Z` tag. It validates
the tag/draft/target and safely re-runs the exact-tag performance chain.

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

Install the Release Please App with Administration:read, Contents:write,
Issues:write, Pull requests:write, and Workflows:write. Store
`RELEASE_PLEASE_CLIENT_ID` and `RELEASE_PLEASE_PRIVATE_KEY` once as repository
Actions secrets; both `release-please.yml` and the publication job in
`release.yml` consume that same source. Do not duplicate them as environment
secrets. Keep the `release-please` environment restricted to `main` with no
required reviewers so version maintenance remains unattended. Keep the
protected `release` environment's independent `sign`/`publish` approvals and its
four eSigner secrets.

The elevated self-hosted runner carries the `fmf-perf` label. Repository
variable `FMF_PERF_BASELINE_DIR` points to its persistent, machine-bound
Criterion baseline outside the checkout; the workflow rejects a missing,
in-checkout, dirty, toolchain-drifted, or cross-machine baseline and exposes
only a per-attempt scratch copy to repository code.

Design rationale lives in ADR-0020, ADR-0029, ADR-0034, ADR-0035, ADR-0038, and
ADR-0040.
