# ADR-0029: CI signing pipeline — official SSL.com Action, build/sign/publish split, approval gate

Date: 2026-06-25 / Status: Accepted (supersedes the *structure* around ADR-0020's signing step; provider/cert unchanged)

> Filename keeps its original `-cka-` slug for link stability; the eSigner CKA approach this ADR first proposed was **tried in CI and reverted** (see "eSigner CKA: attempted and reverted"). The accepted decision is the **official `SSLcom/esigner-codesign` Action** inside a hardened pipeline.

The signing *provider and certificate* stay exactly as ADR-0020 decided: SSL.com eSigner, personal Individual Validation cert `CN=Yasunobu Sakashita`. This ADR changes the *pipeline shape and hardening* around the signing step, and records why the eSigner CKA alternative was rejected after a CI trial.

## Decision

1. **Sign with the official `SSLcom/esigner-codesign` Action (`command: batch_sign`).** This is SSL.com's recommended GitHub Actions integration: the Action downloads CodeSignTool, runs `scan_code` (pre-signing malware scan) then signs, and timestamps via SSL.com's TSA. We sign only our own five PEs, so we stage them into a flat dir (path→unique-name map to dodge the two same-named `FindMyFiles.exe`), `batch_sign` into an explicit `output_path` (the Action ignores `override`), and copy back. The Action is **SHA-pinned** (v1.3.2).

2. **Split `release.yml` into three jobs — `build` → `sign` → `publish` — passing the bundle as an artifact.** Only `sign` ever sees the signing secrets; `build` and `publish` run with a read-only / least-privilege token on a bundle they receive. This shrinks the credential blast radius.

3. **Gate the secrets behind an approval-gated `release` GitHub Environment on the `sign` job.** The eSigner secrets are Environment secrets (not repo-level), with required reviewers and deployment refs restricted to `v*.*.*` + `main`, so a `workflow_dispatch` from an arbitrary ref (or a compromised workflow) cannot mint signatures unattended.

4. **Verify with `signtool verify /pa /tw` + a signer-subject assertion.** `/tw` makes a missing timestamp a non-zero exit (0 = chain valid + timestamped, 2 = untimestamped, 1 = invalid); the subject check (`*CN=Yasunobu Sakashita*`) refuses a valid-but-wrong certificate. `Get-AuthenticodeSignature.TimeStamperCertificate` is **not** used for the timestamp check — it is null under `-FilePath` on the runner (PowerShell#4060), so the timestamp guarantee comes from `signtool`. (CodeSignTool always timestamps, so `/tw` is green.) This is stricter than the prior `Status -eq 'Valid'`-only verify.

5. **Concurrency guard** (`group: release-${{ github.ref }}`, `cancel-in-progress: false`) so two tag pushes never race and a run is never cancelled mid-sign/mid-publish.

Signing stays **non-blocking** (secrets absent → `::warning::`, build still publishes unsigned) and **tag-driven only** (`ci.yml` does not sign).

## Rationale

- **Official Action over CKA**: the Action is SSL.com's documented, supported CI integration and is **proven to sign with this exact account** (it signed successfully before this work; CKA never did — see below). It is SHA-pinnable for supply-chain integrity. CodeSignTool sends only file hashes to SSL.com (source never leaves the runner) and timestamps automatically.
- **Three jobs over one**: defense in depth. A compromised SBOM tool or build step in `build` cannot read signing secrets it never receives; `publish`'s write/attestation token never coexists with the signing credentials.
- **`signtool /tw` over `TimeStamperCertificate`**: the runner's PowerShell returns a null timestamper under `-FilePath`, so asserting it would false-fail; `signtool` exit codes are authoritative.

## eSigner CKA: attempted and reverted

The CKA (Cloud Key Adapter) was attempted to replace the Java CodeSignTool with the standard `signtool` and drop the copy-back dance. It **failed in CI across three dry runs** and was reverted:

- **Cert not visible across steps** (run `28117792530`): a split load-step → sign-step left signtool with "No certificates were found…". Merging load+sign into one shell (run `28119208195`) did not fix it.
- **x64 signtool cannot load the 32-bit `eSignerKSP`** (run `28119208195`): the cert was in `CurrentUser\My` with `HasPrivateKey=True`, yet x64 signtool still reported "No certificates were found". Switching to x86 signtool got past that.
- **KSP credential retrieval fails at sign time** (run `28120321041`, x86): `Signing credentials not configured. Make sure certificate is issued before signing` / `SignerSign() failed (0x80090003)`. This is a CKA-internal CSC credential path, **not** an account problem.

Crucially, the **official Action's `batch_sign` succeeded on the same account/cert** in run `28082306344` (`scan_code` → sign → Verify all green). So the account, PIN, and eSigner credentials are fully provisioned; only the CKA KSP path is the odd one out. This is the prior CKA proposal's own re-examination trigger ("CKA proves flaky in CI → revisit the Action") firing. The 3-job split, approval gate, hardened verify, and concurrency guard — all independent of the signing tool — were **kept**.

## Rejected alternatives

- **eSigner CKA + standard signtool** — would drop the copy-back and use the canonical `signtool`, but **fails in CI** (KSP credential retrieval, above) while the official Action works. Rejected on evidence. The copy-back dance is a small, well-commented price for a proven mechanism.
- **Migrate to SignPath (managed, GitHub-native)** — arguably the most "modern managed" experience and free for OSS, but it is a provider migration with its own onboarding/review and strands the already-purchased SSL.com IV cert. Rejected: no benefit that justifies abandoning a working, paid-for cert.
- **Migrate to Azure Trusted Signing + `dotnet sign`** — the genuine industry standard, but **unavailable**: individual onboarding is paused and new tenants are limited to US/CA orgs with 3+ years of history (ADR-0020; RESEARCH.md). Not a choice for a Japanese individual.
- **`dotnet sign` against SSL.com** — `dotnet sign` only delegates to Azure Key Vault / Trusted Signing; it cannot drive eSigner. Technically incompatible.

## Consequences

- `HAVE_SIGNING` keys on `ES_USERNAME` + `CREDENTIAL_ID` (batch_sign needs the credential_id). The `release` environment holds four secrets: `ES_USERNAME` / `ES_PASSWORD` / `CREDENTIAL_ID` / `ES_TOTP_SECRET`.
- Every release run pauses for reviewer approval before `sign`. A `publish=false` `workflow_dispatch` is a safe signing smoke test (build + sign + verify, no Release) under immutable releases.
- The bundle round-trips through Actions artifacts twice (build→sign→publish); a few extra minutes on a release-only workflow. The Authenticode signature lives inside the PE, so the round-trip preserves it.
- docs/SIGNING.md is the runbook (Environment setup, secrets, renewal); ADR-0020 keeps the provider/cert rationale with a pointer here.
- A future MSIX (ADR-0028) can be signed by the same Action (`sign`/`batch_sign` accept `.msix`).

## Re-examination triggers

- **Azure Trusted Signing opens to individuals in Japan** (or an eligible org is formed) → re-evaluate the whole provider per ADR-0020's trigger; `dotnet sign` + OIDC would then be reachable.
- **eSigner CKA fixes the KSP credential path** (or SSL.com documents a working unattended CKA recipe) → the canonical `signtool` flow becomes worth revisiting to drop the copy-back.
- **MSIX shipping (ADR-0028) lands** → fold its signing into this same Action step.
- **Artifact round-trip cost or a single-platform regret** → collapse back toward fewer jobs (the split's value is the secret isolation, not job count).
