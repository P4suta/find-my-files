# Code Signing — Authenticode Signing of Distributables

Runbook for Authenticode-signing the project's own binaries (`FindMyFiles.exe` and others) in the
distribution zip with **SSL.com eSigner**, via the official **`SSLcom/esigner-codesign` Action**
(`command: batch_sign`). For the provider/cert rationale see [ADR-0020](adr/0020-code-signing-provider.md);
for the pipeline shape (build/sign/publish split, approval gate, hardened verify) and why the eSigner CKA
alternative was rejected, see [ADR-0029](adr/0029-ci-signing-cka-pipeline.md).

## Current state

**Active.** A personal **Individual Validation (IV)** code-signing certificate was obtained from SSL.com on
2026-06-24 (`CN=Yasunobu Sakashita`, code-signing EKU verified), so `.github/workflows/release.yml`
**Authenticode-signs the binaries of each release** (built from the release commit release-please dispatches; the
`vX.Y.Z` tag is created at publish time, it is not the trigger).

Signing is **gated at publish** by design: a real publish (`publish=true`) re-verifies the bundle's five PE files
are validly signed (chain + RFC 3161 timestamp + the expected `CN=Yasunobu Sakashita` signer) and **fails before
creating the immutable Release** if they are not — so a missing or misconfigured signing secret can never ship an
unsigned release unnoticed. A `publish=false` signing smoke test still runs to completion **unsigned, emitting a
`::warning::`** (it creates no Release). The activation procedure below is retained as the runbook for renewal and
re-issue.

Only the **project's own PE files** are signed — the root launcher `FindMyFiles.exe` (the executable the user
double-clicks = the main target of SmartScreen evaluation), the apphost `app\FindMyFiles.exe`, and the engine
binaries `app\fmf.exe`, `app\fmf-service.exe`, `app\fmf_engine.dll`. The bundled .NET / WindowsAppSDK runtime DLLs
are already Microsoft-signed and are not re-signed (to avoid wasting the signing quota and signing others'
copyrighted works).

## How it works (pipeline shape)

`release.yml` runs three jobs so the signing credentials touch the smallest surface (ADR-0029):

1. **build** — `just publish` assembles the bundle + SBOMs and uploads them as artifacts. **No secrets**, read-only token.
2. **sign** — downloads the bundle, stages our five PEs, runs the **`SSLcom/esigner-codesign` Action** (`batch_sign`:
   CodeSignTool scans then signs, timestamping via SSL.com's TSA), copies the signed files back, then hard-verifies.
   **This is the only job that sees the signing secrets**, so it runs in the approval-gated **`release` environment**.
3. **publish** — downloads the signed bundle, **re-verifies it is signed** (a gate at the irreversible boundary —
   the same chain + timestamp + signer check the sign job runs, via a shared composite action, so an unsigned bundle
   can never be published), `just package` (zip + `SHA256SUMS.txt`), writes keyless attestations (OIDC, no secrets)
   and attaches everything to the release-please draft, then publishes it. Gated on `publish=true`.

Two of the five PEs share the basename `FindMyFiles.exe` (the root launcher and the `app\` apphost), so the sign job
stages them through a path→unique-name map into `sign-stage\`, `batch_sign`s them into `signed\`, and copies them back
(Authenticode lives inside the PE, so the staging rename is safe). Verification then enforces a valid chain, an RFC 3161
timestamp, and the expected signer:

```text
signtool verify /pa /tw <file>     # exit 0 = chain valid + timestamped; 2 = no timestamp; 1 = invalid
Get-AuthenticodeSignature <file>   # SignerCertificate.Subject must contain CN=Yasunobu Sakashita
```

## Background (why this setup)

- The official `SSLcom/esigner-codesign` Action is SSL.com's recommended GitHub Actions integration and is **proven to
  sign with this account**. The eSigner **CKA + standard `signtool`** alternative was tried to drop the staging/copy-back,
  but it **fails in CI** at the KSP credential-retrieval step (`SignerSign() 0x80090003`) while the Action's `batch_sign`
  succeeds — see [ADR-0029](adr/0029-ci-signing-cka-pipeline.md) "eSigner CKA: attempted and reverted".
- The 2026 industry-standard managed flow (**Azure Trusted Signing + `dotnet sign` + OIDC**) is **unavailable**: Trusted
  Signing **paused individual onboarding** and now admits only US/CA organizations with 3+ years of history, and
  `dotnet sign` only delegates to Azure Key Vault / Trusted Signing (never SSL.com).
- **EV no longer grants immediate SmartScreen trust** (Microsoft changed this in March 2024). This app ships no kernel
  driver, so EV brings almost no practical benefit; individual-name **IV** is sufficient.
- SmartScreen is reputation-based. Even when signed, **a warning may appear on first run** and disappears as download
  history accumulates. The immediate effect of signing is that "unknown publisher" disappears and **your name** appears.

## Activation procedure (kept for renewal / re-issue)

> Completed 2026-06-24 for the current certificate. Follow these steps again only when renewing or re-issuing.

### A. Obtain the certificate (SSL.com)

1. Create an account at [SSL.com](https://www.ssl.com/).
2. Purchase a **Code Signing** certificate with **Individual Validation (IV) + eSigner (cloud signing)** support (the
   cloud version, not the USB-token version). Expect roughly $130–250 per year.
   - Only if you want the EV title, choose **Sole Proprietor EV** (no corporate registration). **No CI changes** (same
     Action, same secrets); SmartScreen behavior is the same as IV.

### B. Identity verification (IV validation)

3. Government-issued ID + identity verification (documents/video). **No corporate registration required.** Japanese
   individuals / sole proprietors have a track record of obtaining it.

### C. Configure eSigner for automated signing

4. In the SSL.com dashboard, on the certificate order:
   - Configure the **eSigner PIN / signing secret** and enable automated signing.
   - Note the **Credential ID** of the signing certificate.
   - Issue and note the **TOTP (2FA) secret for automated signing** (a Base32 string).
   - The account **username / password**.

### D. Register the signing secrets in the `release` environment

The secrets live in an **approval-gated GitHub Environment**, not at repository level, so a `workflow_dispatch` from an
arbitrary ref (or a compromised workflow) cannot mint signatures unattended (ADR-0029).

5. Repository → **Settings → Environments → New environment** → name it **`release`**.
6. **Required reviewers**: add yourself (so every signing run pauses for a deliberate approval). **Deployment branches
   and tags → Selected**: allow `main`. Under the draft-first model release.yml always runs via `workflow_dispatch
   --ref main` (both real releases and `publish=false` dry runs), so `main` is the only ref that ever deploys to this
   environment — a policy that omits it would deny the real `sign`/`publish` jobs.
7. **Environment secrets** (Add secret), on the `release` environment — **all four** are required by `batch_sign`:

   | Secret name | Value |
   |---|---|
   | `ES_USERNAME` | SSL.com username |
   | `ES_PASSWORD` | SSL.com password |
   | `CREDENTIAL_ID` | Credential ID of the signing certificate |
   | `ES_TOTP_SECRET` | TOTP secret for eSigner automated signing (Base32) |

   If any of these already exist as **repository** secrets, **delete the repository copies** — leaving them at repo
   level defeats the environment isolation.

   → On the next release (the `release: approved` Release PR merged → release-please dispatches release.yml) or a
   manual `workflow_dispatch`, `HAVE_SIGNING` becomes `true` and the `sign` job requests approval, then signs.

### E. Verification

8. **Signing smoke test (safe under immutable releases)**: run Actions → release via `workflow_dispatch` with
   `tag_name=main`, `publish=false`. The `build` job runs, then the **`sign` job pauses for approval** (the environment
   gate). Approve it; confirm the **Sign staged binaries** step runs `scan_code` → sign and the **Verify signatures**
   step prints `verified: … - CN=Yasunobu Sakashita (chain+timestamp+signer OK)` for all five files, and the run **ends
   cleanly after `sign`** (no Release, because `publish=false`).
9. **Real release**: merge the `release: approved` Release PR — release-please creates the draft and dispatches
   release.yml automatically (or dispatch it by hand with a plain `tag_name=vX.Y.Z`, no pre-release suffix, and
   `publish=true`). After both approvals, `build`→`sign`→`publish` runs end-to-end and the signed zip +
   `SHA256SUMS.txt` + SBOMs + attestations are attached to the published release.
10. **Local confirmation**: extract the Release zip and on Windows verify the root launcher (the rest — apphost +
    engine binaries — sit under `app\`):
    ```powershell
    signtool verify /pa /tw /v FindMyFiles.exe       # → Successfully verified, with a timestamp
    Get-AuthenticodeSignature FindMyFiles.exe         # → Status: Valid
    Get-AuthenticodeSignature app\FindMyFiles.exe     # the apphost → Status: Valid
    ```
    In the properties of `FindMyFiles.exe` → "Digital Signatures" tab, your name and a timestamp appear.

## Renewal (handling expiry)

- The validity period of a publicly trusted code-signing certificate is, per CA/Browser Forum rules, **at most ~460
  days (about 15 months)**. Renew at SSL.com before expiry.
- **Only if the Credential ID / TOTP change** on renewal, update the corresponding Environment secret. If the
  certificate **subject** ever changes (different name), update `SIGNER_SUBJECT_CONTAINS` in `release.yml` too — it is
  asserted at verify time.

## Troubleshooting

- **`sign` job never starts**: it is waiting on the `release` environment's required reviewer — approve the run from the
  Actions page.
- **Sign step fails with `hash needs to be scanned first before submitting for signing`**: the SSL.com account has the
  pre-signing malware blocker enabled, so `batch_sign` cannot sign hashes that were never scanned. The fix is already
  wired: `malware_block: "true"` makes the Action scan the files inline before signing. (Only flip to `false` for MSIX
  inputs, which SSL.com requires be unscanned.)
- **Verify fails with `NotSigned`**: `batch_sign` does not honor `override`, so the signed files must be written to an
  explicit `output_path` — otherwise copy-back grabs the unsigned originals. This is already wired (`output_path: …\signed`).
- **`verify failed … chain invalid or NOT timestamped`**: `signtool verify /pa /tw` returned non-zero — the SSL.com TSA
  may have been unreachable during signing, or the chain did not resolve. Re-run; a timestamp is required so signatures
  survive cert expiry.
- **`unexpected signer …`**: the signed cert subject does not contain `SIGNER_SUBJECT_CONTAINS` (renewal under a new
  name?). Update the env var in `release.yml`.
- **A SmartScreen warning still appears on first launch**: expected (reputation is shallow). It disappears as downloads
  accumulate. Same with EV.

## Related

- [ADR-0020 — Code Signing Provider Selection](adr/0020-code-signing-provider.md) (provider / cert)
- [ADR-0029 — CI signing pipeline (official Action, build/sign/publish, approval gate)](adr/0029-ci-signing-cka-pipeline.md)
- [SECURITY.md](SECURITY.md)
- Wiring itself: `.github/workflows/release.yml`
