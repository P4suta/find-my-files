# ADR-0029: CI signing pipeline — eSigner CKA + standard signtool, build/sign/publish split

Date: 2026-06-25 / Status: Accepted (supersedes the signing *mechanism* of ADR-0020; the provider/cert choice in ADR-0020 is unchanged)

The signing *provider and certificate* stay exactly as ADR-0020 decided: SSL.com eSigner, personal Individual Validation cert `CN=Yasunobu Sakashita`. This ADR only changes **how** `release.yml` drives that certificate and **how** the release pipeline is shaped, to move from a bespoke wrapper toward the canonical Windows signing flow.

## Context

The first implementation signed via the `SSLcom/esigner-codesign` Action's Java `batch_sign`, wrapped in PowerShell that staged our five PEs into a directory (path→unique-name map to dodge the two same-named `FindMyFiles.exe`), batch-signed into a separate `signed/` dir (because `batch_sign` ignores `override`), copied them back, then verified. It worked, but the stage→sign→copy-back dance and a single monolithic job read as "homemade CI".

The 2026 industry-standard managed flow (**Azure Trusted Signing + `dotnet sign` + OIDC**, no stored secrets, auto-managed short-lived certs) is **unreachable here**: Trusted Signing **paused individual onboarding** and now admits only US/CA organizations with 3+ years of verifiable history (a Japanese individual cannot enroll — RESEARCH.md), and `dotnet sign` only delegates to Azure Key Vault / Trusted Signing, never SSL.com. So "most modern" is constrained; the reachable improvement is to make the signing *operation* canonical and isolate the secrets.

## Decision

1. **Sign with the standard `signtool` via eSigner CKA (Cloud Key Adapter), not the Java CodeSignTool Action.** CKA loads the cloud certificate into the Windows store as a virtual token; we then sign in place with `signtool sign /fd sha256 /tr http://ts.ssl.com /td sha256 /sha1 <thumbprint>`. Full paths sign the two same-named `FindMyFiles.exe` directly, so the staging + copy-back is gone. Timestamping is an explicit `signtool` argument (RFC 3161, SHA-256), so signatures outlive the ~460-day cert.

2. **Split `release.yml` into three jobs — `build` → `sign` → `publish` — passing the bundle as an artifact.** Only `sign` ever sees the signing secrets; `build` and `publish` run with a read-only / least-privilege token on a bundle they receive. This shrinks the credential blast radius and reads as a standard release pipeline.

3. **Gate the secrets behind an approval-gated `release` GitHub Environment on the `sign` job.** The eSigner secrets become Environment secrets (not repo-level), with required reviewers and deployment refs restricted to `v*.*.*` + `main`, so a `workflow_dispatch` from an arbitrary ref (or a compromised workflow) cannot mint signatures unattended.

4. **Verify with `signtool verify /pa /tw` + a signer-subject assertion.** `/tw` makes a missing timestamp a non-zero exit (0 = chain valid + timestamped, 2 = untimestamped, 1 = invalid), and the subject check (`*CN=Yasunobu Sakashita*`) refuses a valid-but-wrong certificate. `Get-AuthenticodeSignature.TimeStamperCertificate` is **not** used for the timestamp check — it is null under `-FilePath` on the runner (PowerShell#4060), so the timestamp guarantee comes from `signtool`.

5. **Pin the CKA installer by release-asset URL + committed SHA-256.** The CKA is fetched at runtime (it is not a SHA-pinnable Action), so the download is checksum-verified before it runs; this restores the supply-chain integrity that the pinned Action gave. (Version/hash live in `release.yml` env + docs/SIGNING.md.)

Signing stays **non-blocking** (secrets absent → `::warning::`, build still publishes unsigned) and **tag-driven only** (`ci.yml` does not sign), exactly as before.

## Rationale

- **CKA + signtool over the Action**: it is the canonical Windows signing path (the same `signtool` everyone uses), eliminates the bespoke copy-back, and makes timestamping explicit. The one cost — the CKA installer is downloaded at runtime rather than a SHA-pinned Action — is bought back with a committed SHA-256 gate (decision 5).
- **Three jobs over one**: defense in depth. A compromised SBOM tool or build step in `build` cannot read signing secrets it never receives; `publish`'s write/attestation token never coexists with the signing credentials.
- **`signtool /tw` over `TimeStamperCertificate`**: the runner's PowerShell returns a null timestamper under `-FilePath`, so asserting it would false-fail; `signtool` exit codes are authoritative.

## Consequences

- The signing-secret gate moves from "presence of repo secrets" to "presence of Environment secrets in the approval-gated `release` environment". `HAVE_SIGNING` now keys on `ES_USERNAME` + `ES_TOTP_SECRET` (CKA is TOTP-driven; the legacy `CREDENTIAL_ID` is no longer required by the signing step).
- Every release run now pauses for reviewer approval before `sign`. A `publish=false` `workflow_dispatch` is a safe signing smoke test (build + sign + verify, no Release) under immutable releases.
- The bundle round-trips through Actions artifacts twice (build→sign→publish); a few extra minutes of upload/download on a release-only workflow. The Authenticode signature lives inside the PE, so the artifact round-trip preserves it.
- The same CKA + signtool path can sign a future MSIX (ADR-0028) — one signing mechanism for both channels.
- docs/SIGNING.md is the runbook (Environment setup, secret migration, CKA version+hash, renewal); ADR-0020 keeps the provider/cert rationale with a pointer here.

## Rejected alternatives

- **Keep the `SSLcom/esigner-codesign` Action (+ just modernize structure)** — the Action is fully SHA-pinned (a real supply-chain plus), but it keeps the Java `batch_sign` + stage/copy-back wrapper that motivated this change. Rejected for the bespoke feel; the CKA's runtime download is mitigated by the SHA-256 pin.
- **Migrate to SignPath (managed, GitHub-native)** — arguably the most "modern managed" experience and free for OSS, but it is a provider migration with its own onboarding/review, and it strands the already-purchased SSL.com IV certificate. Rejected: no benefit that justifies abandoning a working, paid-for cert.
- **Migrate to Azure Trusted Signing + `dotnet sign`** — the genuine industry standard, but **unavailable**: individual onboarding is paused and new tenants are limited to US/CA orgs with 3+ years of history (ADR-0020; RESEARCH.md). Not a choice for a Japanese individual.
- **`dotnet sign` against SSL.com** — `dotnet sign` only delegates to Azure Key Vault / Trusted Signing; it cannot drive eSigner. Rejected as technically incompatible.

## Re-examination triggers

- **eSigner CKA proves flaky in CI** (cert-store load races, installer changes) → revisit the `SSLcom/esigner-codesign` Action (still SHA-pinnable) for the signing step while keeping the 3-job split + Environment gate.
- **Azure Trusted Signing opens to individuals in Japan** (or an eligible org is formed) → re-evaluate the whole provider per ADR-0020's trigger; `dotnet sign` + OIDC would then be reachable.
- **MSIX shipping (ADR-0028) lands** → fold its signing into this same CKA + signtool step rather than a parallel mechanism.
- **Artifact round-trip cost or a single-platform regret** → collapse back toward fewer jobs (the split's value is the secret isolation, not job count).
