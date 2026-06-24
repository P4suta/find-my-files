# ADR-0020: Code-signing provider selection (SSL.com eSigner / individual IV)

Date: 2026-06-13 / Status: Accepted (active — IV certificate obtained 2026-06-24; eSigner secrets registered. Runbook docs/SIGNING.md)

Certificate holder: `CN=Yasunobu Sakashita` (SSL.com individual IV, code-signing EKU), issued via `SSL.com Code Signing Intermediate CA RSA R1`.

> **Update (2026-06-25):** the **provider and certificate decided here are unchanged**, but the CI *integration mechanism* has moved on — see [ADR-0029](0029-ci-signing-cka-pipeline.md). `release.yml` now signs with the standard `signtool` via the eSigner **CKA** (not the Java `batch_sign` Action), in a `build`→`sign`→`publish` job split with the secrets behind an approval-gated `release` environment. The rationale below (why SSL.com eSigner / IV over Azure / EV / Certum / SignPath) still stands; the paragraphs about `batch_sign` and the staging map describe the original mechanism, now superseded by ADR-0029. Azure Trusted Signing has since **paused individual onboarding** entirely (US/CA orgs, 3+ years only), reinforcing the rejection below.

## Decision

Authenticode signing of the distributed binaries is done with **SSL.com eSigner** (a cloud HSM signing service) + a **personal Individual Validation (IV)
certificate**. Signing is kept as a **CI-environment-specific YAML step** in `release.yml` (tag-driven), not placed in `xtask/`.
Signing is **non-blocking** by design (if Secrets are unset, finish unsigned + `::warning::`); it stayed dormant until the certificate was obtained and is now active.

The signing targets are **only our own PEs** — 5 since the bundle gained a root launcher (see ADR-0021): the root launcher
`FindMyFiles.exe`, the apphost `app\FindMyFiles.exe`, `app\fmf.exe`, `app\fmf-service.exe`, `app\fmf_engine.dll`. The bundled
.NET / WindowsAppSDK runtime DLLs are Microsoft-signed, so they are not re-signed. (Two of the five share the basename
`FindMyFiles.exe`, so `release.yml` stages them via a path→unique-name map before batch-signing.)

## Rationale

- **Azure Artifact Signing (formerly Trusted Signing) not adopted**: it is managed and easy to integrate into CI (`release.yml` was
  originally wired to this service), but as of 2026 the **personal tier is limited to US/CA/EU/UK**, and **individuals residing in Japan cannot apply**. Eliminated by the geographic requirement.
- **EV not adopted (IV adopted)**: since March 2024, EV **no longer grants instant SmartScreen trust** (Microsoft official).
  SmartScreen is purely reputation-based — reputation accrues from the signer certificate + file hash via download history — and "first-time warning -> cleared by track record" is
  the same for EV/OV/IV. This app **does not ship a kernel driver** (do-not-do list), so EV's remaining practical benefits (driver signing, corporate procurement requirements) do not apply.
  **IV**, the cheapest and obtainable under a personal name, is the rational choice. The budget (100,000 yen/year) puts EV in range too, but the consideration is "title only".
- **SSL.com eSigner adopted**: cloud HSM signing needs no hardware token on the runner. Fully unattended CI signing via TOTP.
  A GitHub Action (`SSLcom/esigner-codesign`) exists. It supports both **personal IV** and **Sole Proprietor EV** (no corporate registration required), and
  is obtainable from Japan. Best fit for the "fully outsourced managed signing" requirement.
  - The alternative Certum personal (about $50/15 months) is cheapest but SimplySign requires a phone OTP per signature, which is a **poor fit for unattended CI**.
    SignPath Foundation (FOSS, free) requires review and may put new projects on hold. Both are inferior on the "throw-it-over-the-wall managed" requirement.
- **Keep signing as a YAML step (not in xtask)**: signing is CI-environment-specific processing that depends on GitHub Secrets and an Action; it is not
  the "portable release procedure logic" that `xtask/` consolidates. Follows the precedent set by the Azure version (a YAML step).
- **Sign in-house PEs only**: re-signing MS runtime DLLs wastes eSigner quota and is meaningless signing of others' copyrighted work. Collect just the 4 in a staging directory,
  `batch_sign` (1 OTP), and after copy-back **hard-verify** with `Get-AuthenticodeSignature` (do not silently succeed unsigned when signing was requested = the "do not stay silent" principle).

## Consequences

- The signing step in `release.yml` has already been swapped from Azure to SSL.com eSigner. The gate `HAVE_SIGNING` is
  decided by the presence of `ES_USERNAME` + `CREDENTIAL_ID`. The 4 Secrets were registered 2026-06-24, so **signing is active from the next tag** (docs/SIGNING.md, section D).
- Publicly trusted certificates expire after at most ~460 days (CA/Browser Forum 2026). Renewal procedure is in docs/SIGNING.md.
- Signing is **limited to the tag-driven `release.yml`**. `ci.yml` (PR/push) does not sign (do not distribute development intermediates, conserve quota, fork PRs
  cannot access Secrets).

## Re-examination triggers

- If Azure Artifact Signing opens to **individuals in Japan**, re-evaluate on managed-ness and CI affinity.
- If this project comes to have a **kernel driver**, EV becomes a mandatory requirement.
- If a **corporate EV procurement requirement** (enterprise distribution, store requirements, etc.) arises, reconsider Sole Proprietor EV / corporate EV.
- If SmartScreen's reputation model changes and first-time behavior again differs by signing type, revisit.
