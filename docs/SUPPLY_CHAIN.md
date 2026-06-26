# Supply Chain and Provenance

The mechanisms and verification procedures that let users machine-verify that a distributable was
"built from this commit of this repository, by an untampered CI." For code signing (Authenticode), see
[SIGNING.md](SIGNING.md). This document covers **build provenance (SLSA provenance), SBOM, and dependency pinning**.

## For users: verify a download

`release.yml` (tag-driven) issues GitHub-native keyless attestation. There is **no private key**; it signs to
Sigstore (Fulcio/Rekor) with the workflow's OIDC token. All you need to verify is `gh`:

```
# Verify build provenance (which commit / workflow / runner built it)
gh attestation verify find-my-files-vX.Y.Z-win-x64.zip --repo P4suta/find-my-files

# Verify that the SBOM is bound to the same zip (CycloneDX predicate)
gh attestation verify find-my-files-vX.Y.Z-win-x64.zip --repo P4suta/find-my-files \
  --predicate-type https://cyclonedx.org/bom
```

Successful verification means "the artifact's digest matches an attestation issued from `P4suta/find-my-files`'s
`release.yml`." The release attaches the following:

| Asset | Contents |
|---|---|
| `find-my-files-vX.Y.Z-win-x64.zip` | App + engine binaries (Authenticode-signed when signing is enabled) |
| `SHA256SUMS.txt` | SHA-256 of the zip |
| `fmf-engine.cdx.json` | SBOM of the Rust engine (CycloneDX 1.6. `cargo-sbom`, all workspace dependencies) |
| `app.cdx.json` | SBOM of the C# app (CycloneDX 1.6. CycloneDX dotnet tool, NuGet graph) |

The zip and SHA256SUMS have a build-provenance attestation, and each SBOM has an SBOM attestation
(listed in the repository's **Attestations** tab; 3 total).

## Dependency and build controls

| Aspect | Mechanism |
|---|---|
| Rust dependency lock | `engine/Cargo.lock` / `xtask/Cargo.lock` (committed) |
| C# dependency lock | `app/FindMyFiles/packages.lock.json` / `app/FindMyFiles.Tests/packages.lock.json`. CI treats stale as failure via `-p:RestoreLockedMode=true` |
| Vulnerabilities (source tree) | `cargo-audit` (RustSec, weekly + on lock change). C# uses CodeQL + Dependabot |
| Vulnerabilities (shipped releases) | `osv-scanner` consumes the release SBOMs: a **release gate** in `release.yml` (blocks publishing a build with a known-vulnerable dep in the resolved closure — incl. the .NET graph `cargo-audit` can't see) and a **weekly re-scan** of the latest release's SBOM (`sbom-monitor.yml`) that catches advisories disclosed *after* shipping. Accepted/unfixable advisories: `osv-scanner.toml`. See [ADR-0034](adr/0034-sbom-consumed-by-osv-scanner.md) |
| License/provenance | `cargo-deny` (bans / licenses / sources. Unknown registries and git are deny) |
| Auto-update | Dependabot (cargo / nuget×2 / github-actions. Weekly) |
| Action pinning | Third-party actions in all workflows are pinned to a **40-char commit SHA** (with `# vX.Y.Z` alongside). Dependabot updates the SHA and comment. `actionlint` validates workflows in the hygiene job |
| Posture monitoring | OpenSSF Scorecard (weekly, SARIF to the Security tab, README badge) |
| Reproducible build | C# uses `ContinuousIntegrationBuild=true` in CI (embedded source path normalization. `Deterministic` is the SDK default). Rust is deterministic by default |

## SBOMs are consumed, not just attached (ADR-0034)

The SBOM isn't a write-only release artifact — `osv-scanner` (OSV.dev) reads it at two points:

1. **Release gate** — `release.yml`'s `build` job scans both `*.cdx.json` right after generating them, before sign/publish. A known-vulnerable dependency in the resolved shipped closure fails the build. This catches the **.NET/NuGet graph**, which `cargo-audit` (Rust-only, reads `Cargo.lock`) never sees.
2. **Shipped-release re-scan** — `sbom-monitor.yml` runs weekly, downloads the **latest release's** attested SBOMs, and re-scans them against the current OSV DB. This is the only check covering *what users already downloaded*: a CVE disclosed after a release is invisible to the source-tree scanners (which only see HEAD).

When the weekly re-scan finds something it opens (or updates) a single issue labelled **`sbom-vuln`** with the osv-scanner report; once the affected release is clean again the issue is auto-closed. With no published release yet the job is a clean no-op and activates on the first release. Accepted/unfixable advisories go in **`osv-scanner.toml`** at the repo root (the OSV counterpart to `engine/deny.toml`), honoured by both the gate and the monitor.

## For maintainers: runbook for the first attested release

`release.yml` runs three jobs — `build` → `sign` → `publish` — and the attestation/SBOM steps live in `publish`,
which fires only when publishing. The `sign` job pauses for approval (the `release` environment). So:

1. **Signing-only dry-run** (safe under immutable releases): run `release` via **`workflow_dispatch`** with
   `tag_name=main`, `publish=false`. Approve the `sign` job; confirm `build`+`sign`+verify pass and the run ends with
   **no Release** (the `publish` job is skipped). See [SIGNING.md](SIGNING.md) §F.
2. **Attestation/OIDC dry-run**: to exercise the `publish` job (`id-token: write` / `attestations: write`), run
   `workflow_dispatch` with a throwaway `tag_name` and `publish=true`. Note immutable releases make a published
   release non-deletable, so use a tag you are happy to keep.
3. For production, run `just release` as usual (version bump + signed tag push) → `release.yml` fires automatically;
   approve the `sign` job when prompted.
4. After completion, confirm that `gh attestation verify <zip> --repo P4suta/find-my-files` succeeds, the
   **Attestations** tab has 3 items (provenance + SBOM×2), and the release has zip / SHA256SUMS / `*.cdx.json`.

### Notes

- **SBOM tools are CI/release-only** (not added to the `mise.toml` development loop). For Rust,
  `cargo install cargo-sbom`; for C#, `dotnet tool install --global CycloneDX` (both version-pinned). Both languages
  standardize on **CycloneDX 1.6**.
- For lock file updates, Dependabot's nuget PR regenerates `packages.lock.json`. After adding a version locally,
  run `dotnet restore` (both csproj) → commit. The floating `4.*` of `Roslynator.Analyzers` is pinned to the
  resolved version by the lock file, so on bump the lock file must be regenerated (= the intended determinism).
- Optional future extension: `Microsoft.SourceLink.GitHub` (makes PDBs traceable to commits). Deferred because it
  adds dependency surface. Consider adding it if there is debugging demand for distributed PDBs.
