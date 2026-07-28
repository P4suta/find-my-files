# ADR-0020: Code-signing provider selection (SSL.com eSigner / individual IV)

Date: 2026-06-13 / Status: Accepted (active — IV certificate obtained 2026-06-24; workflow is the executable runbook)

Certificate holder: `CN=Yasunobu Sakashita` (SSL.com individual IV, code-signing EKU), issued via `SSL.com Code Signing Intermediate CA RSA R1`.

> **Update (2026-06-25):** the **provider, certificate, and signing Action decided here are unchanged** — `release.yml` still signs with the official `SSLcom/esigner-codesign` Action (`batch_sign`) and the staging map described below. What changed is the *pipeline around it*: see [ADR-0029](0029-ci-signing-cka-pipeline.md) — a `build`→`sign`→`package`→`publish` split with the secrets behind an approval-gated `release` environment, read-only packaging, write/OIDC-only publication, and a hardened verify (`signtool /pa /tw` timestamp + signer-CN). (An eSigner CKA + standard `signtool` mechanism was trialled to drop the staging/copy-back but **failed in CI** and was reverted — ADR-0029.) Azure Trusted Signing has since **paused individual onboarding** entirely (US/CA orgs, 3+ years only), reinforcing the rejection below.

## Decision

Authenticode signing of the distributed binaries is done with **SSL.com eSigner** (a cloud HSM signing service) + a **personal Individual Validation (IV)
certificate**. Signing is kept as a **CI-environment-specific YAML step** in the trusted-`main`, reusable-only `release.yml`, not placed in `xtask/`; the Release Please tag and commit are validated inputs, never the workflow source.
A real release is **fail-closed**: all four signing secrets, a valid signature, timestamp, and expected signer are mandatory. There is no credentialed unsigned rehearsal path.

The signing targets are **only our own PEs**, including the managed application assembly. Their executable manifest lives in `xtask` and is mirrored by `.github/actions/verify-signatures/first-party-pes.txt`; counts are deliberately not duplicated in this ADR. Bundled .NET / WindowsAppSDK runtime DLLs retain their Microsoft signatures. Same-named first-party files are staged through unique names before batch signing.

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
- **Sign in-house PEs only**: re-signing MS runtime DLLs wastes eSigner quota and is meaningless signing of others' copyrighted work. Collect the manifest-defined set in a staging directory,
  `batch_sign` (1 OTP), and after copy-back **hard-verify** chain/timestamp with `signtool /pa /tw` plus signer identity with `Get-AuthenticodeSignature` (do not silently succeed unsigned when signing was requested = the "do not stay silent" principle).

## Consequences

- The signing step in `release.yml` uses SSL.com eSigner. `HAVE_SIGNING` requires `ES_USERNAME`, `ES_PASSWORD`, `CREDENTIAL_ID`, and `ES_TOTP_SECRET`; `publish=true` fails before signing if any is absent.
- Publicly trusted certificates expire after at most ~460 days (CA/Browser Forum 2026). Renewal updates the `release` environment secrets when the credential or TOTP changes.
- Signing is **limited to reusable `release.yml` calls from the protected-`main`
  controller after exact tag/SHA performance evidence**. `ci.yml` (PR/push) does not sign (do not distribute development intermediates, conserve quota, fork PRs
  cannot access Secrets).

## Re-examination triggers

- If Azure Artifact Signing opens to **individuals in Japan**, re-evaluate on managed-ness and CI affinity.
- If this project comes to have a **kernel driver**, EV becomes a mandatory requirement.
- If a **corporate EV procurement requirement** (enterprise distribution, store requirements, etc.) arises, reconsider Sole Proprietor EV / corporate EV.
- If SmartScreen's reputation model changes and first-time behavior again differs by signing type, revisit.
