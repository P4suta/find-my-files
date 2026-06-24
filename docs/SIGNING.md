# Code Signing — Authenticode Signing of Distributables

Runbook for Authenticode-signing the project's own binaries (`FindMyFiles.exe` and others) in the
distribution zip with **SSL.com eSigner**. The certificate is loaded into the runner's Windows store by the
**eSigner CKA (Cloud Key Adapter)** and signed with the standard `signtool`. For the provider/cert rationale
see [ADR-0020](adr/0020-code-signing-provider.md); for the CKA + `signtool` mechanism and the
`build`→`sign`→`publish` pipeline shape see [ADR-0029](adr/0029-ci-signing-cka-pipeline.md).

## Current state

**Active.** A personal **Individual Validation (IV)** code-signing certificate was obtained from SSL.com on
2026-06-24 (`CN=Yasunobu Sakashita`, code-signing EKU verified), so `.github/workflows/release.yml`
**Authenticode-signs the binaries from each `vX.Y.Z` tag**.

Signing remains **non-blocking** by design: if the signing secrets (`ES_USERNAME` / `ES_TOTP_SECRET`) were ever
removed, cutting a tag would still **complete, leaving the binaries unsigned and emitting a `::warning::`** rather
than failing. The activation procedure below is retained as the runbook for renewal and re-issue.

Only the **project's own PE files** are signed — the root launcher `FindMyFiles.exe` (the executable the user
double-clicks = the main target of SmartScreen evaluation), the apphost `app\FindMyFiles.exe`, and the engine
binaries `app\fmf.exe`, `app\fmf-service.exe`, `app\fmf_engine.dll`. The bundled .NET / WindowsAppSDK runtime DLLs
are already Microsoft-signed and are not re-signed (to avoid wasting the signing quota and signing others'
copyrighted works).

## How it works (pipeline shape)

`release.yml` runs three jobs so the signing credentials touch the smallest surface (ADR-0029):

1. **build** — `just publish` assembles the bundle + SBOMs and uploads them as artifacts. **No secrets**, read-only token.
2. **sign** — downloads the bundle, acquires the **eSigner CKA** (checksum-pinned), loads the cloud cert into the
   Windows store, signs our five PEs in place with `signtool` (RFC 3161 timestamp), then hard-verifies. **This is the
   only job that sees the signing secrets**, so it runs in the approval-gated **`release` environment**.
3. **publish** — downloads the signed bundle, `just package` (zip + `SHA256SUMS.txt`), writes keyless attestations
   (OIDC, no secrets) and attaches everything to the Release. Gated on a tag push / `publish=true`.

The signing step itself is the canonical Windows flow:

```text
signtool sign /fd sha256 /tr http://ts.ssl.com /td sha256 /sha1 <thumbprint> <file>
signtool verify /pa /tw <file>     # exit 0 = chain valid + timestamped; 2 = no timestamp; 1 = invalid
```

## Background (why this setup)

- The 2026 industry-standard managed flow (**Azure Trusted Signing + `dotnet sign` + OIDC**) is **unavailable**: Trusted
  Signing **paused individual onboarding** and now admits only US/CA organizations with 3+ years of history, and
  `dotnet sign` only delegates to Azure Key Vault / Trusted Signing (never SSL.com). So we drive the SSL.com cert with
  the standard `signtool` via CKA instead (ADR-0029).
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
     CKA, same secrets); SmartScreen behavior is the same as IV.

### B. Identity verification (IV validation)

3. Government-issued ID + identity verification (documents/video). **No corporate registration required.** Japanese
   individuals / sole proprietors have a track record of obtaining it.

### C. Configure eSigner for automated signing

4. In the SSL.com dashboard:
   - Issue and note the **TOTP (2FA) secret for automated signing** (a Base32 string) — this is what drives CKA.
   - The account **username / password**.

### D. Register the signing secrets in the `release` environment

The secrets live in an **approval-gated GitHub Environment**, not at repository level, so a `workflow_dispatch` from an
arbitrary ref (or a compromised workflow) cannot mint signatures unattended (ADR-0029).

5. Repository → **Settings → Environments → New environment** → name it **`release`**.
6. **Required reviewers**: add yourself (so every signing run pauses for a deliberate approval). **Deployment branches
   and tags → Selected**: allow `v*.*.*` (releases) and `main` (the `publish=false` dry run).
7. **Environment secrets** (Add secret), on the `release` environment:

   | Secret name | Value |
   |---|---|
   | `ES_USERNAME` | SSL.com username |
   | `ES_PASSWORD` | SSL.com password |
   | `ES_TOTP_SECRET` | TOTP secret for eSigner automated signing (Base32) |

   If any of these already exist as **repository** secrets, **delete the repository copies** — leaving them at repo
   level defeats the environment isolation. (`CREDENTIAL_ID` is no longer used by the signing step and can be removed.)

   → On the next `vX.Y.Z` tag (or `release` via `workflow_dispatch`), `HAVE_SIGNING` becomes `true` and the `sign` job
   requests approval, then signs.

### E. CKA installer pin

The CKA is downloaded at runtime and **checksum-verified** before it runs (it is not a SHA-pinnable Action). The pin
lives in `release.yml` (`CKA_URL` / `CKA_SHA256`). Current pin:

| Field | Value |
|---|---|
| Version | eSigner CKA **v1.0.7** |
| Asset | `SSL.COM-eSigner-CKA_1.0.7.zip` (15,290,082 bytes) |
| SHA-256 | `0F40F0EF0AA5C7D73B2D854EC0D2F2BE551A6BBBD99CBBD886F7D3EF77C3327C` |

To bump: download the new release asset, compute `Get-FileHash -Algorithm SHA256`, and update **both** `CKA_URL` and
`CKA_SHA256` in `release.yml` and this table together.

### F. Verification

8. **Signing smoke test (safe under immutable releases)**: run Actions → release via `workflow_dispatch` with
   `tag_name=main`, `publish=false`. The `build` job runs, then the **`sign` job pauses for approval** (the environment
   gate). Approve it; confirm the **Verify signatures** step prints `verified: … - CN=Yasunobu Sakashita
   (chain+timestamp+signer OK)` for all five files and the run **ends cleanly after `sign`** (no Release, because
   `publish=false`).
9. **Real release**: cut a tag (e.g. `v0.0.1-rc1`). After approval, `build`→`sign`→`publish` runs end-to-end and the
   signed zip + `SHA256SUMS.txt` + SBOMs + attestations are attached.
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
- **Only if the username / password / TOTP change** on renewal, update the corresponding Environment secret. If the
  certificate **subject** ever changes (different name), update `SIGNER_SUBJECT_CONTAINS` in `release.yml` too — it is
  asserted at verify time.

## Troubleshooting

- **`sign` job never starts**: it is waiting on the `release` environment's required reviewer — approve the run from the
  Actions page.
- **`expected signing certificate … not in store after CKA load`**: the CKA loaded a cert whose subject does not contain
  `SIGNER_SUBJECT_CONTAINS`. Check the account's cert (renewal under a new name?) and update the env var.
- **`verify failed … chain invalid or NOT timestamped`**: `signtool verify /pa /tw` returned non-zero — the SSL.com TSA
  (`http://ts.ssl.com`) may have been unreachable during signing, or the chain did not resolve. Re-run; a timestamp is
  required so signatures survive cert expiry.
- **`eSigner CKA checksum mismatch`**: the pinned asset changed or the download was corrupted. Verify the asset/hash in
  section E and update the pin if SSL.com legitimately re-published.
- **`signtool.exe not found on runner`**: the Windows SDK layout changed; the step already falls back from `x64` to
  `x86`. If both fail, install the SDK explicitly in the `sign` job.
- **A SmartScreen warning still appears on first launch**: expected (reputation is shallow). It disappears as downloads
  accumulate. Same with EV.

## Related

- [ADR-0020 — Code Signing Provider Selection](adr/0020-code-signing-provider.md) (provider / cert)
- [ADR-0029 — CI signing pipeline (CKA + signtool, build/sign/publish)](adr/0029-ci-signing-cka-pipeline.md) (mechanism)
- [SECURITY.md](SECURITY.md)
- Wiring itself: `.github/workflows/release.yml`
