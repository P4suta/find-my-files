# ADR-0034: SBOMs are consumed by osv-scanner — release gate + shipped-release re-scan

Date: 2026-06-26 / Status: Accepted (extends ADR-0029's SBOM generation with downstream consumption)

ADR-0029 made `release.yml` generate and attest CycloneDX SBOMs (Rust via `cargo-sbom`, C# via the CycloneDX dotnet tool) and attach them to the release. But nothing *consumed* them: `cargo-audit` reads `Cargo.lock`, `cargo-deny` reads `Cargo.lock`, C# vulnerabilities go through CodeQL + Dependabot — none touch the SBOM. The SBOM was a write-only artifact whose only value was provenance/attestation and an OpenSSF Scorecard "Secured release" tick. "You can trace it — so what?" had no answer.

This ADR gives the SBOM a job by feeding it to **`osv-scanner`** (OSV.dev) at two points.

## Decision

1. **Release gate (`release.yml`, `build` job).** Right after the two SBOMs are generated — before sign/publish — run `osv-scanner scan source -L fmf-engine.cdx.json -L app.cdx.json --config osv-scanner.toml`. A finding (exit 1) fails the build, so a release with a known-vulnerable dependency in the **resolved shipped closure** never publishes. This is non-redundant with `cargo-audit`: the C#/.NET NuGet graph (resolved for `win-x64`) is only captured in the SBOM and is invisible to `cargo-audit`.

2. **Shipped-release re-scan (`sbom-monitor.yml`, weekly + `workflow_dispatch`).** Download the **latest** release's attested SBOMs and re-scan them against the *current* OSV DB. This is the only check that covers *what users already downloaded*: `cargo-audit` / `cargo-deny` / Dependabot all scan HEAD, so a CVE disclosed after a release is invisible for the shipped binary. The frozen, attested SBOM is exactly the manifest needed to answer "is the shipped `vX.Y.Z` affected?".

3. **Report findings as a single idempotent issue, not SARIF.** The monitor opens/updates one issue labelled `sbom-vuln` (auto-closed when the release is clean again). We deliberately do **not** upload SARIF to Code Scanning — that surface was just decluttered (the stale-CodeQL cleanup), and "a shipped release is vulnerable → cut a patch release" is an assignable/closable *task*, which an issue models better than a code-scanning alert (which is about current code).

4. **`osv-scanner.toml` at the repo root is the single ignore list.** Accepted/unfixable advisories are recorded there (the OSV counterpart to `engine/deny.toml`), honoured by both the gate and the monitor, so an upstream advisory with no fix can't permanently block releases or spam the issue. Every entry must justify itself.

5. **Tooling: `osv-scanner` via the already-trusted `taiki-e/install-action` (SHA-pinned), version-pinned `@2.3.6`.** Not added to `mise.toml` — like the SBOM generators, it's CI/release-only (SUPPLY_CHAIN.md), so the dev loop stays untouched.

## Rationale

- **osv-scanner over grype / trivy / bomber**: osv-scanner consumes CycloneDX natively, covers **both** crates.io and NuGet against one DB (OSV.dev) in a single pass, is Google-maintained, SHA-pinnable via an action the repo already uses, and uses simple exit codes (0 clean / 1 findings / 128 no-packages). grype/trivy are heavier and container-oriented; bomber is narrower. No reason to add a second ecosystem.
- **Consume the SBOM rather than scan lockfiles again**: scanning lockfiles would just duplicate `cargo-audit` (Rust) and skip the .NET closure. Scanning the *SBOM* is what makes the artifact earn its keep and is the only way to reach the resolved NuGet/runtime graph and the *shipped* (not HEAD) state.
- **Gate fail-fast in `build`**: the SBOM exists there, and failing before the approval-gated `sign` job wastes no reviewer time and never signs a vulnerable bundle.
- **Dormant-first**: no release exists yet, so the monitor must no-op cleanly (notice + exit 0) rather than fail red, then activate automatically on the first release. Non-blocking scaffolding wired ahead of the external/irreversible event (first release).

## Rejected alternatives

- **Leave the SBOM as provenance-only + document it** — honest and near-zero effort, but leaves the "so what?" unanswered and the unique shipped-release-monitoring gap open. Rejected: the gap is real and the cost to close it is small.
- **Drop SBOM generation entirely** — would remove a cargo-cult artifact, but loses the Scorecard "Secured release" credit and the one genuinely useful capability (a frozen manifest for retrospective CVE response). Rejected against the project's security-hardening posture.
- **SARIF → Code Scanning** instead of an issue — integrates with the Security tab but re-clutters the surface just cleaned, and models "current code finding" rather than "shipped release needs a patch". Rejected for the monitor (the release gate needs neither).
- **Scan a matrix of all supported releases** — correct at scale, but this is a solo, pre-1.0 project with one release line; "latest" is the whole supported surface. Deferred (see trigger).
- **Add osv-scanner to `mise.toml`** — would unify dev/CI, but it's never run in the dev loop; keeping it CI-only matches the SBOM tools and avoids dev-loop surface.

## Consequences

- A release can now be **blocked** by an OSV finding in either ecosystem; the escape hatch is a justified `osv-scanner.toml` entry (not disabling the gate).
- Partial overlap with `cargo-audit` on the Rust side is accepted: the gate adds the .NET closure, and the monitor adds the shipped-vs-HEAD dimension neither `cargo-audit` nor Dependabot provide.
- One new weekly workflow (cheap ubuntu) and one extra `build`-job step on releases. The monitor needs `issues: write`; the gate adds no new permissions.
- The `sbom-vuln` issue is the maintainer's signal that a shipped release needs a patch release.

## Re-examination triggers

- **Multiple supported release lines** (post-1.0, LTS branches) → extend the monitor to a matrix of supported tags instead of "latest".
- **osv-scanner false positives or upstream-unfixable noise grows** → the `osv-scanner.toml` list balloons; revisit gate strictness (gate-warn vs gate-fail) or per-ecosystem policy.
- **A second SBOM consumer becomes useful** (license posture from the SBOM, VEX statements) → fold into the same osv-scanner config rather than adding a tool.
