# ADR-0029: CI signing pipeline — official SSL.com Action, privilege-separated release jobs

Date: 2026-06-25 / Status: Accepted (supersedes the *structure* around ADR-0020's signing step; provider/cert unchanged)

> Filename keeps its original `-cka-` slug for link stability; the eSigner CKA approach this ADR first proposed was **tried in CI and reverted** (see "eSigner CKA: attempted and reverted"). The accepted decision is the **official `SSLcom/esigner-codesign` Action** inside a hardened pipeline.

The signing *provider and certificate* stay exactly as ADR-0020 decided: SSL.com eSigner, personal Individual Validation cert `CN=Yasunobu Sakashita`. This ADR changes the *pipeline shape and hardening* around the signing step, and records why the eSigner CKA alternative was rejected after a CI trial.

## Decision

1. **Sign with the official `SSLcom/esigner-codesign` Action (`command: batch_sign`).** This is SSL.com's recommended GitHub Actions integration: the Action downloads CodeSignTool, runs `scan_code` (pre-signing malware scan) then signs, and timestamps via SSL.com's TSA. A fresh no-checkout job copies exactly five protected-workflow literal paths from the sealed bundle into a flat directory; the credentialed job signs only that five-file artifact into an explicit `output_path` (the Action ignores `override`). The fixed map is deliberately repeated at this credential boundary so target repository code cannot turn the certificate into a signing oracle. The Action is **SHA-pinned** (v1.3.2).

2. **Split `release.yml` into eight jobs — `build` → `sbom` → `sign-stage` → `sign` → `sign-collect` → `package` → `publish-approval` → `publish` — using immutable Actions artifacts at each boundary.** Only `sign` sees signing secrets and it executes no repository code. `sbom` scans disposable copies while preserving the sealed unsigned bundle. `publish-approval` is a secretless second human decision. `publish` receives only the completed zip/checksum/SBOM pair plus both bundle manifests. On a fresh no-checkout runner, protected inline validation binds the full source commit, controller commit, numeric Release ID, performance run, four public assets, and both manifests into a custom keyless attestation before an App token publishes that exact draft ID. No toolchain or repository build code shares the write/OIDC boundary.

3. **Gate the secrets behind an approval-gated `release` GitHub Environment on the `sign` job.** The eSigner secrets are Environment secrets (not repo-level), with required reviewers and deployment refs restricted to protected `main` only. The credentialed pipeline is reusable-only from the default-branch `workflow_run` controller; its tag and commit are validated data, never executable workflow source.

4. **Verify with `signtool verify /pa /tw` plus exact certificate identity.** `/tw` makes a missing timestamp a non-zero exit (0 = chain valid + timestamped, 2 = untimestamped, 1 = invalid); the verifier pins the common name, full subject, issuer, and certificate SHA-256. `Get-AuthenticodeSignature.TimeStamperCertificate` is **not** used for the timestamp check — it is null under `-FilePath` on the runner (PowerShell#4060), so the timestamp guarantee comes from `signtool`.

5. **Concurrency guard** (`group: release-stable-publication`, `cancel-in-progress: false`) so stable publications never race and a run is never cancelled mid-sign/mid-publish.

Signing is **fail-closed for publication**. The credentialed workflow has no unsigned rehearsal entry point; `ci.yml` never signs.

## Rationale

- **Official Action over CKA**: the Action is SSL.com's documented, supported CI integration and is **proven to sign with this exact account** (it signed successfully before this work; CKA never did — see below). It is SHA-pinnable for supply-chain integrity. CodeSignTool sends only file hashes to SSL.com (source never leaves the runner) and timestamps automatically.
- **Separated privilege stages over one**: defense in depth. A compromised build/SBOM step cannot read signing secrets; repository package code receives no write token; the publish write/OIDC token sees only a strict artifact allowlist and protected inline validation plus pinned official attestation/token Actions.
- **`signtool /tw` over `TimeStamperCertificate`**: the runner's PowerShell returns a null timestamper under `-FilePath`, so asserting it would false-fail; `signtool` exit codes are authoritative.

## eSigner CKA: attempted and reverted

The CKA (Cloud Key Adapter) was attempted to replace the Java CodeSignTool with the standard `signtool` and drop the copy-back dance. It **failed in CI across three dry runs** and was reverted:

- **Cert not visible across steps** (run `28117792530`): a split load-step → sign-step left signtool with "No certificates were found…". Merging load+sign into one shell (run `28119208195`) did not fix it.
- **x64 signtool cannot load the 32-bit `eSignerKSP`** (run `28119208195`): the cert was in `CurrentUser\My` with `HasPrivateKey=True`, yet x64 signtool still reported "No certificates were found". Switching to x86 signtool got past that.
- **KSP credential retrieval fails at sign time** (run `28120321041`, x86): `Signing credentials not configured. Make sure certificate is issued before signing` / `SignerSign() failed (0x80090003)`. This is a CKA-internal CSC credential path, **not** an account problem.

Crucially, the **official Action's `batch_sign` succeeded on the same account/cert** in run `28082306344` (`scan_code` → sign → Verify all green). So the account, PIN, and eSigner credentials are fully provisioned; only the CKA KSP path is the odd one out. This is the prior CKA proposal's own re-examination trigger ("CKA proves flaky in CI → revisit the Action") firing. The privilege-separated jobs, approval gate, hardened verify, and concurrency guard — all independent of the signing tool — were **kept**.

## Rejected alternatives

- **eSigner CKA + standard signtool** — would drop the copy-back and use the canonical `signtool`, but **fails in CI** (KSP credential retrieval, above) while the official Action works. Rejected on evidence. The copy-back dance is a small, well-commented price for a proven mechanism.
- **Migrate to SignPath (managed, GitHub-native)** — arguably the most "modern managed" experience and free for OSS, but it is a provider migration with its own onboarding/review and strands the already-purchased SSL.com IV cert. Rejected: no benefit that justifies abandoning a working, paid-for cert.
- **Migrate to Azure Trusted Signing + `dotnet sign`** — the genuine industry standard, but **unavailable**: individual onboarding is paused and new tenants are limited to US/CA orgs with 3+ years of history (ADR-0020; RESEARCH.md). Not a choice for a Japanese individual.
- **`dotnet sign` against SSL.com** — `dotnet sign` only delegates to Azure Key Vault / Trusted Signing; it cannot drive eSigner. Technically incompatible.

## Consequences

- `HAVE_SIGNING` requires all four `release` environment secrets: `ES_USERNAME` / `ES_PASSWORD` / `CREDENTIAL_ID` / `ES_TOTP_SECRET`.
- Every release run pauses before `sign` and again at the secretless
  `publish-approval`; the environments accept only protected `main`. A signing
  rehearsal, if reintroduced, must be a separate credentialless workflow rather
  than a second mode of the production release pipeline.
- The sealed bundle/assets cross immutable Actions-artifact boundaries between build, SBOM, signing, packaging, and publication; a few extra minutes on a release-only workflow. Every transition re-verifies the expected file set and content identity. The Authenticode signature lives inside the PE, so the round-trips preserve it.
- `.github/workflows/release.yml` is the executable runbook; the irreducible human approvals are summarized in `docs/RELEASING.md`.
- A future MSIX (ADR-0028) can be signed by the same Action (`sign`/`batch_sign` accept `.msix`).

## Re-examination triggers

- **Azure Trusted Signing opens to individuals in Japan** (or an eligible org is formed) → re-evaluate the whole provider per ADR-0020's trigger; `dotnet sign` + OIDC would then be reachable.
- **eSigner CKA fixes the KSP credential path** (or SSL.com documents a working unattended CKA recipe) → the canonical `signtool` flow becomes worth revisiting to drop the copy-back.
- **MSIX shipping (ADR-0028) lands** → fold its signing into this same Action step.
- **Artifact round-trip cost or a single-platform regret** → collapse back toward fewer jobs (the split's value is the secret isolation, not job count).
