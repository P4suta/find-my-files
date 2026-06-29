# ADR-0038: Build identity discoverability in the shipped artifact

Date: 2026-06-30 / Status: Accepted (no wire-contract / golden / ABI change; radiates the existing `FMF_BUILD_VERSION` / fmf-buildstamp identity to the artifact surface)

## Context

The dev / nightly / stable build lanes were complete ([ADR-0035](0035-automated-versioning-with-release-please-and-build-channels.md)): one format authority (`xtask version`) computes a channel-aware string, CI exports it as `FMF_BUILD_VERSION`, and the Rust binaries (`fmf-buildstamp::VERSION`) and the C# app (`InformationalVersion`) stamp it. The single source of truth was clean — but it **did not reach the surface of a downloaded artifact**. A user who downloaded a build could not tell, at a glance, which channel/version it was:

1. **The zip filename was the only signal**, and it is lost the moment the zip is extracted (the bundle folder is always `FindMyFiles`, `paths.rs`).
2. **No in-bundle version file** — the bundle shipped only an instructional `README.txt`, with no version/channel/commit/date.
3. **The root `FindMyFiles.exe` (the launcher) carried no real version resource** — `winresource` defaulted it to the internal crate name `fmf-launcher` at a static `0.1.0.0`, identical across all channels (misleading).
4. **No in-app version display** — the GUI showed only the engine version (F12, pipe-only); the app's own version was reachable only via the F12 "copy diagnostics" dump.
5. **`SHA256SUMS.txt` was non-standard** — a bare uppercase hash with no filename (mirroring PowerShell `Get-FileHash`), so the ubiquitous `sha256sum -c SHA256SUMS.txt` could not verify it.
6. **Inconsistencies in the version identity itself**: `fmf diag` reported the bare `CARGO_PKG_VERSION` (no channel/sha), disagreeing with `fmf --version`; the C# `InformationalVersion` carried Source Link's full 40-char sha with no `g` prefix and even leaked `+sha` into stable, diverging from the Rust `+g<7>` / clean-stable shape.

## Decision

Radiate the **existing** build identity (no new version source) to four artifact surfaces, following industry-standard mechanisms, and fix the identity inconsistencies so every surface agrees.

1. **In-bundle `BUILDINFO.txt`** (the strongest at-a-glance, survives extraction). `xtask publish` writes a Notepad-friendly, grep-able `key: value` file (product, version, channel, commit, date, source, license) beside `README.txt`. The version uses the **same precedence as the binaries** (`FMF_BUILD_VERSION` else local `-dev+g<sha>`); the date is the git commit date (reproducible, no wall clock), with the nightly's embedded date preferred. Parsing/rendering is pure and unit-tested in `xtask/src/version.rs` (`parse_identity` / `render_buildinfo`).
2. **Launcher Win32 VERSIONINFO.** `fmf-launcher/build.rs` sets the resource via `winresource`: numeric `FileVersion = X.Y.Z.0` (Win32 requires `a.b.c.d`) and string `ProductVersion = FMF_BUILD_VERSION` (the channel-aware value), plus `ProductName=FindMyFiles`, description, copyright and source URL — so Explorer → Properties → Details identifies the build without running it.
3. **In-app About / version block.** The Settings dialog's Status section shows the app version (always, selectable to copy) and the engine version (pipe mode), and raises a warning InfoBar when their `X.Y.Z` bases differ (`BuildInfo.SameBase`) — surfacing a stale app/service pairing that nothing previously detected.
4. **Standardised release artifacts.** `SHA256SUMS.txt` moves to coreutils format (lowercase hash, two spaces, filename), directory-driven over `build/package`, verifiable with `sha256sum -c`. The nightly Actions artifact is named with its date (`find-my-files-nightly-<date>`).
5. **Identity consistency.** `fmf diag` now reports `fmf_buildstamp::VERSION` (matches `--version`). The C# side disables Source Link's auto-append (`IncludeSourceRevisionInInformationalVersion=false`) and constructs `+g<short7>` itself via an MSBuild target, exactly mirroring `xtask version` (`+g<7>`; stable stays clean).

The change flow stops short of the contract: `fmf-contract` / `fmf-proto` / `contract/golden` are untouched (no wire/ABI/golden change).

## Rationale

- **Radiate, don't add a source.** Every surface derives from the one `FMF_BUILD_VERSION` / fmf-buildstamp value; the format authority remains `xtask version`. This preserves the [ADR-0035](0035-automated-versioning-with-release-please-and-build-channels.md) single-source discipline — no surface can drift.
- **`BUILDINFO.txt` over relying on the zip name.** The filename is the strongest signal *until* extraction, after which a plain-text file is the only thing that survives — and it doubles as machine-readable (`key: value`), consistent with the project's logfmt direction ([ADR-0037](0037-logfmt-diagnostics-and-correlation.md)).
- **coreutils `SHA256SUMS` is the de-facto standard.** No stable release had shipped (`.release-please-manifest.json` = `0.0.0`), so there were no consumers of the old uppercase/no-filename shape to break — the right moment to standardise.
- **Mismatch detection is cheap and real.** Both sides already stamp the same fmf-buildstamp shape, so comparing the `X.Y.Z` base is trivial and catches a genuine support-time problem (which app is talking to which service).

## Trade-off

The launcher's dev fallback (~5 lines: `FMF_BUILD_VERSION` else `-dev+g<sha>`) is duplicated in `fmf-launcher/build.rs` and `fmf-buildstamp/build.rs`. Build scripts cannot share a runtime const, and a shared leaf crate for five lines is over-engineering; the duplication is annotated with a cross-reference and the format authority stays in `xtask version`. The C# short-sha is resolved at MSBuild target-execution time (the sha is unknowable at property-evaluation time); when git is absent (source tarball) it falls back to the channel tag without a sha, mirroring the Rust `None` branch. The C# side does not stamp `.dirty` (a local-only Rust nicety), matching `xtask version` rather than the Rust local default.

## Rejected alternatives

- **A shared `fmf-buildmeta` leaf crate** used as a build-dependency by both build scripts: rejected — the only real duplication is the 5-line dev fallback, and parsing lives once in `xtask`; a crate to dedupe five lines fails the dsa-first cost test.
- **Naming the extracted bundle folder with the version** (so the folder itself signals the build): rejected — the zip stores contents at the root (matching the historical `Compress-Archive` shape), and `BUILDINFO.txt` covers the post-extraction case without restructuring the archive.
- **Embedding SBOMs in `SHA256SUMS.txt`**: deferred — SBOMs live in `build/sbom` (not `build/package`) and are integrity-protected by build-provenance attestation (stronger than a sums line); `SHA256SUMS` stays directory-driven over `build/package`, so a future SBOM dropped there is covered automatically.
- **Structured `version=` / `channel=` logfmt fields on every line**: deferred — the version is already on the launch line; first-class fields are a refinement, not part of artifact discoverability, and would touch the freshly-landed logfmt infra.
