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

Install the Release Please App with Administration:read, Contents:write,
Issues:write, and Pull requests:write. Store
`RELEASE_PLEASE_CLIENT_ID` and `RELEASE_PLEASE_PRIVATE_KEY` only in the
`release-please` environment. Restrict it to protected `main`. Keep the
`release` environment restricted to protected `main`, with required reviewers,
admin bypass disabled, and only the four eSigner secrets.

Performance baselines are recorded by hand on the reference machine, cold and
idle: `just bench-baseline` for the real-volume baseline and
`just bench-micro-baseline` for the Criterion suite. The real-volume result
(`engine/benches/baseline.json`) lands through an ordinary reviewed PR; the
Criterion baseline is machine-local. Never hand-edit or fabricate either.

Design rationale lives in ADR-0013, ADR-0020, ADR-0029, ADR-0034, ADR-0035,
ADR-0038, ADR-0040, and ADR-0048.
