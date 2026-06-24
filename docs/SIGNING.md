# Code Signing — Authenticode Signing of Distributables

Runbook for Authenticode-signing the project's own binaries (`FindMyFiles.exe` and others) in the
distribution zip with **SSL.com eSigner** (cloud HSM signing). For the decision rationale and rejected
alternatives, see [ADR-0020](adr/0020-code-signing-provider.md).

## Current state

**Active.** A personal **Individual Validation (IV)** code-signing certificate was obtained from SSL.com on
2026-06-24 (`CN=Yasunobu Sakashita`, code-signing EKU verified) and the 4 eSigner Secrets are registered, so
`.github/workflows/release.yml` **Authenticode-signs the binaries from each `vX.Y.Z` tag**.

Signing remains **non-blocking** by design: if the repository Secrets (`ES_USERNAME` / `CREDENTIAL_ID`) were ever
removed, cutting a tag would still **complete, leaving the binaries unsigned and emitting a `::warning::`** rather
than failing. The activation procedure below is retained as the runbook for renewal and for re-issuing the
certificate. No CI changes are needed to keep signing working.

Only the **project's own PE files** are signed — `FindMyFiles.exe` (the executable the user launches = the main
target of SmartScreen evaluation), `fmf.exe`, `fmf-service.exe`, `fmf_engine.dll`. The bundled .NET / WindowsAppSDK
runtime DLLs are already Microsoft-signed and are not re-signed (to avoid wasting the signing quota and signing
others' copyrighted works).

## Background (why this setup)

- An **individual residing in Japan** is **not eligible** for the individual tier of Azure Artifact Signing
  (formerly Trusted Signing) (US/CA/EU/UK only).
- **EV signing no longer grants immediate SmartScreen trust** (Microsoft changed this in March 2024). This app
  ships no kernel driver, so taking EV brings almost no practical benefit. Therefore individual-name
  **IV (Individual Validation)** is sufficient.
- SmartScreen is reputation-based. Even when signed, **a warning may appear on first run** and disappears as
  download history accumulates. The immediate effect of signing is that "unknown publisher" disappears and
  **your name** appears in the properties.

## Activation procedure (kept for renewal / re-issue)

> Completed 2026-06-24 for the current certificate. Follow these steps again only when renewing or re-issuing.

### A. Obtain the certificate (SSL.com)

1. Create an account at [SSL.com](https://www.ssl.com/).
2. Purchase a **Code Signing** certificate. Choose **Individual Validation (IV) with eSigner (cloud signing)**
   support (the cloud version, not the USB token version). Expect roughly $130–250 per year.
   - Only if you want the EV title, you may choose **Sole Proprietor EV** (no corporate registration required).
     **No changes to this repository's CI** (same Action, same 4 Secrets). But SmartScreen behavior is the same
     as IV.

### B. Identity verification (IV validation)

3. Government-issued ID + identity verification (documents/video). **No corporate registration required.** There
   is a track record of Japanese individuals / sole proprietors obtaining it.

### C. Configure eSigner for automated signing

4. In the SSL.com dashboard:
   - Note the **Credential ID** of the signing certificate.
   - Issue and note the **TOTP (2FA) secret for automated signing** (a Base32 string).
   - The account **username / password**.

### D. Register 4 GitHub Secrets

5. In the repository → Settings → Secrets and variables → Actions → New repository secret:

   | Secret name | Value |
   |---|---|
   | `ES_USERNAME` | SSL.com username |
   | `ES_PASSWORD` | SSL.com password |
   | `CREDENTIAL_ID` | Credential ID of the signing certificate |
   | `ES_TOTP_SECRET` | TOTP secret for eSigner automated signing (Base32) |

   → On the next `vX.Y.Z` tag (or `release` via `workflow_dispatch`), `HAVE_SIGNING` becomes `true` and signing runs.

### E. Verification

6. **Dry run (only relevant before the Secrets are registered)**: with Secrets unset, run Actions → release
   via `workflow_dispatch` → confirm that the signing step is skipped, a `::warning::` is emitted, and the
   zip / checksum / Release creation **complete without failure** (= the non-blocking wiring does not break the pipeline).
7. **Real signing (after registering Secrets)**: cut a test tag (e.g. `v0.0.1-rc1`) and run. Confirm that
   "Sign staged binaries" runs and "Verify signatures" turns green with all 4 files showing
   `signed: ... - CN=<your name>`.
8. **Local confirmation**: extract the Release zip and on Windows:
   ```powershell
   signtool verify /pa /v build\dist\FindMyFiles\FindMyFiles.exe   # → Successfully verified
   Get-AuthenticodeSignature build\dist\FindMyFiles\FindMyFiles.exe # → Status: Valid
   ```
   In the properties of `FindMyFiles.exe` → "Digital Signatures" tab, your name and a timestamp appear.

## Renewal (handling expiry)

- The validity period of a publicly trusted code signing certificate is, per CA/Browser Forum rules,
  **at most ~460 days (about 15 months)**. Renew at SSL.com before expiry.
- **Only if the Credential ID / TOTP change** on renewal, update the corresponding Secret.

## Troubleshooting

- **Sign step fails with `hash needs to be scanned first before submitting for signing`**: the SSL.com account has
  the pre-signing malware blocker enabled, so `batch_sign` cannot sign hashes that were never scanned. The fix is
  already wired in `release.yml`: `malware_block: "true"` makes the Action scan the files inline before signing.
  (If you ever sign MSIX inputs, SSL.com requires the scan be **disabled** for those — flip it back to `false`.)
- **Verify signatures fails with `NotSigned`**: `batch_sign` does not honor `override`, so the signed files must be
  written to an explicit `output_path` — otherwise "Copy signed binaries back" copies the unsigned originals. This is
  already wired: the sign step sets `output_path` to a `signed/` dir and copy-back reads from there (not `sign-stage/`).
- **`Get-AuthenticodeSignature` returns `UnknownError`**: the public trust chain is unresolved. Check details with
  `signtool verify /pa`.
- **A SmartScreen warning still appears on first launch**: expected (reputation is shallow). It disappears as
  downloads accumulate. Same with EV.

## Related

- [ADR-0020 — Code Signing Provider Selection](adr/0020-code-signing-provider.md)
- [SECURITY.md](SECURITY.md)
- Wiring itself: `.github/workflows/release.yml`
