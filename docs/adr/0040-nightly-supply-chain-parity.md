# ADR-0040: Nightly carries full supply-chain provenance (signing stays stable-only)

Date: 2026-06-30 / Status: Accepted (CI-only; no contract/golden/ABI change)

## Context

[ADR-0035](0035-automated-versioning-with-release-please-and-build-channels.md) §5 made nightly an unsigned 14-day GitHub Actions artifact (not a Release), and [ADR-0029](0029-ci-signing-cka-pipeline.md) keeps Authenticode signing tag-driven and approval-gated (`release` environment), so signing is deliberately stable-only. That part is industry-normal.

But auditing the channels showed the supply-chain gap was wider than signing. Only `release.yml` generated CycloneDX SBOMs, ran the osv-scanner gate ([ADR-0034](0034-sbom-consumed-by-osv-scanner.md)), and issued keyless **build-provenance + SBOM attestations**. `nightly.yml` shipped only `SHA256SUMS.txt` — no SBOM, no attestation, no scan. **Nothing in the ADRs justified that gap**; those steps simply only existed in `release.yml`. The documented rationale covered *signing* (eSigner quota + human approval), not provenance.

By SLSA, **every distributed artifact — nightlies included — should carry build provenance**. GitHub's `actions/attest-build-provenance` is keyless (Sigstore Fulcio/Rekor via the workflow OIDC token): no stored secret, no human approval, no eSigner quota. So a nightly could be `gh attestation verify`-able at essentially zero cost, and its absence was an implementation gap, not a decision.

## Decision

Give nightly the **same supply-chain artefacts as a release, minus signing**:

1. **CycloneDX 1.6 SBOMs (Rust + C#) + the osv-scanner gate** run in `nightly.yml`, exactly as in `release.yml`. A known-vulnerable dependency in the resolved closure fails the nightly too (don't ship a vulnerable nightly).
2. **Keyless build-provenance attestation** over the zip + `SHA256SUMS.txt`, and an **SBOM attestation** per SBOM (provenance + 2 × SBOM = 3 attestations), via the build job's `id-token: write` / `attestations: write` permissions. No secrets, no approval gate.
3. The SBOMs are **added to the 14-day artifact** so a tester gets them alongside the zip.
4. **Signing is unchanged** — still tag-driven, approval-gated, stable-only (ADR-0029). A nightly stays unsigned; the only remaining stable-only gate is the Authenticode signature.

To avoid drift, SBOM generation + the osv-scanner gate are extracted into a **composite action** (`.github/actions/sbom-scan`) shared by both `release.yml` (its `build` job) and `nightly.yml`, so the pinned tool versions (`cargo-sbom` 0.10.0, `CycloneDX` 6.2.0, `osv-scanner` 2.3.6) live in one place. Only the non-sensitive `build` job of `release.yml` is touched; its `sign`/`publish` jobs are unchanged. The attestation steps stay in each workflow (they depend on job-level OIDC permissions a composite action can't grant).

## Rationale

- **SLSA expects provenance on every distributed build.** Keyless attestation is free and unattended — there was no cost reason to withhold it from nightly. This closes the real standards gap.
- **Signing is the legitimate stable-only gate**, not SBOM/provenance: eSigner has a quota and a human approval gate (ADR-0029); attestation/SBOM have neither.
- **Composite action over copy-paste**: two workflows pinning `cargo-sbom`/`CycloneDX`/`osv-scanner` independently would drift; one shared action keeps them identical (the project already uses composite actions for single-source, e.g. `rust-toolchain`).
- **Refactor only the `build` job**: the SBOM steps have no secrets and no approval gate, so extracting them carries none of the risk of touching `sign`/`publish`.

## Trade-off

A nightly's SBOM is attached + attested but **not re-scanned by `sbom-monitor.yml`** — that monitor only tracks the latest *Release*, and a nightly artifact expires in 14 days, so post-hoc monitoring of it would be pointless. The osv-scanner gate at build time still applies. The nightly's keyless attestations persist in the repo's Attestations tab even after the artifact expires (harmless, and they remain verifiable for anyone who kept the download).

## Rejected alternatives

- **Sign nightlies too.** Rejected: keeps the eSigner quota + approval-gate cost that ADR-0029/0035 deliberately reserve for stable. The "Signed nightly wanted" trigger in ADR-0035 still governs that, and is now the *only* supply-chain difference between nightly and release.
- **Duplicate the SBOM steps into `nightly.yml`.** Rejected: tool-version drift between the two workflows; the composite action is the single source.
- **Document the gap as intentional and leave nightly checksum-only.** Rejected: it would be documenting a non-decision; SLSA says ship the provenance, and it's free.

## Re-examination triggers

- **Signed nightly wanted** → add a `sign` job to `nightly.yml` (ADR-0035 §re-examination; reuses ADR-0029's pipeline). This is now the sole remaining nightly/release supply-chain gap.
- If a third workflow needs SBOMs, it `uses: ./.github/actions/sbom-scan` (don't re-inline).
