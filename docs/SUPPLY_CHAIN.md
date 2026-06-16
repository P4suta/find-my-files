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
| Vulnerabilities | `cargo-audit` (RustSec, weekly + on lock change). C# uses CodeQL + Dependabot |
| License/provenance | `cargo-deny` (bans / licenses / sources. Unknown registries and git are deny) |
| Auto-update | Dependabot (cargo / nuget×2 / github-actions. Weekly) |
| Action pinning | Third-party actions in all workflows are pinned to a **40-char commit SHA** (with `# vX.Y.Z` alongside). Dependabot updates the SHA and comment. `actionlint` validates workflows in the hygiene job |
| Posture monitoring | OpenSSF Scorecard (weekly, SARIF to the Security tab, README badge) |
| Reproducible build | C# uses `ContinuousIntegrationBuild=true` in CI (embedded source path normalization. `Deterministic` is the SDK default). Rust is deterministic by default |

## For maintainers: runbook for the first attested release

The attestation/SBOM steps fire only on tags, so **do a dry-run before the real tag** to confirm the OIDC/permission path:

1. Manually run `release` via **`workflow_dispatch`** (input `tag_name`) with an existing test tag (or a throwaway tag).
   Confirm that `permissions: id-token: write / attestations: write` and each step pass.
2. For production, run `just release` as usual (version bump + tag push) → `release.yml` fires automatically.
3. After completion, confirm that `gh attestation verify <zip> --repo P4suta/find-my-files` succeeds, the
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
