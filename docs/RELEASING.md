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
5. Approve `sign` and then the secretless `publish-approval` job in the protected
   `release` environment. Confirm the expected `vX.Y.Z` and immutable SHA each
   time. The subsequent API-only publication job obtains App credentials from
   `release-please`.

The workflow chain is the executable specification: it binds the approved tag,
source SHA, draft Release ID, performance evidence, and published assets, and a
stray tag cannot publish. Do not bypass or replay individual downstream jobs.

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
the tag/draft/target, fixes the draft's numeric ID once, and safely re-runs the
trusted-main performance chain against that exact tag commit and Release ID.

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
Issues:write, and Pull requests:write. Store
`RELEASE_PLEASE_CLIENT_ID` and `RELEASE_PLEASE_PRIVATE_KEY` only in the
`release-please` environment. Restrict it to protected `main`. Keep the
`release` environment restricted to protected `main`, with required reviewers,
admin bypass disabled, and only the four eSigner secrets.

The instrument must be the sole runner in organization runner group
`fmf-performance`, with labels `Windows`, `X64`, and `fmf-perf`. Restrict the
group to this repository and exactly
`OWNER/REPO/.github/workflows/performance-controller.yml@refs/heads/main`.
The shared `performance` environment requires a reviewer, disables admin bypass,
allows protected branches only, and contains zero secrets. `just
performance-doctor` audits this live configuration with organization-owner `gh`
credentials. A user-owned repository cannot provide this boundary, so
performance and stable publication intentionally remain unavailable until an
organization migration and audit.

The machine-bound Criterion baseline has one fixed location:
`P:\find-my-files\performance-baseline\criterion`. Its protected
DACL permits only SYSTEM and Administrators. Gate runs copy it to per-attempt
scratch. A baseline request records real-volume and Criterion data on the same
serialized instrument; a hosted API-only job can propose only
`engine/benches/baseline.json` through a draft PR. Never hand-edit or fabricate a
baseline.

Design rationale lives in ADR-0020, ADR-0029, ADR-0034, ADR-0035, ADR-0038, and
ADR-0040.
