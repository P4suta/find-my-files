# find-my-files

**Instant file-name search for Windows, built with a Rust engine and a native WinUI 3 UI.**

[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/P4suta/find-my-files/badge)](https://scorecard.dev/viewer/?uri=github.com/P4suta/find-my-files)

> Status: early releases available — grab the Windows x64 zip from the [Releases](https://github.com/P4suta/find-my-files/releases) page.

**Project page:** [p4suta.github.io/find-my-files](https://p4suta.github.io/find-my-files/) — overview in [日本語](https://p4suta.github.io/find-my-files/) / [English](https://p4suta.github.io/find-my-files/en/)

## What it does

**Windows-only, file names only, FOSS.**

- Initial index by reading the NTFS $MFT directly (~seconds per volume)
- Real-time NTFS updates from the USN journal
- Multithreaded SIMD substring scan over an in-memory index (~100 MB per million files)
- Name order is maintained continuously; size/date order is built lazily and cached
- Native WinUI 3: Mica, consistent dark theme, Per-Monitor V2 DPI (no blur on mixed-DPI setups)

## What it deliberately does NOT do

Content/property indexing, tags, previews, FTP/HTTP servers, FAT/exFAT/network drives (initially).
Indexing file names only is *the* reason it's fast. Feature creep is a non-goal.

## Privilege model

Reading the NTFS Master File Table and USN journal requires elevated volume access.
The first-run button uses one UAC prompt to register a hardened, on-demand
service named `fmf-engine` (`sc query fmf-engine`), run from `fmf-service.exe`.
After that the UI stays unprivileged and connects through an authorized-user
named pipe; it may start or stop only that service, with no right to reconfigure
or delete it. Explicit `--engine=inproc` remains an elevated diagnostic
fallback. See [the security model](docs/SECURITY.md).

By default, hidden/system files — and everything under hidden/system folders
($Recycle.Bin contents, `pagefile.sys`, `.git` internals…) — are excluded from
results. A setting brings them back instantly (they stay indexed).

Only fixed NTFS drives are indexed. ReFS, FAT/exFAT, removable media, and
network drives are outside the current product scope.

`fmf-service uninstall --purge-data` removes only the machine-wide service and
engine data under `%ProgramData%\find-my-files`; it never removes UI settings or
app logs under `%APPDATA%\find-my-files`. To remove both scopes, use **Remove all
Find My Files data** in the app's service-management screen.

## Building

Toolchain is pinned via [mise](https://mise.jdx.dev/) (`mise.toml`), tasks run via `just`:

```
mise install        # pinned toolchain and development tools (including just)
just setup          # git hooks (lefthook)
just doctor         # check the environment matches the pins
just build          # engine (cargo, release)
just service-dev    # run the engine service in the foreground (elevated)
just index C:       # index a volume from the CLI (elevated terminal required)
```

`just --list` is the entry point for every development task; each recipe carries
its own description, so that menu — not a prose guide — is the reference. Run
`just verify` (fmt + lint + Rust/C# tests + dependency gates) before pushing, and
`just perf-gate` in an elevated, cool-machine shell if you touched `fmf-core`.
Do not install project tools ad hoc: pin them in `mise.toml`, then `mise install`.

`fmf --help` is the developer CLI reference. Versions are channel-aware
(`dev`, `nightly`, `stable`) and are derived automatically from
[Conventional Commits](https://www.conventionalcommits.org/) — a lefthook
`commit-msg` hook and a CI PR-title gate enforce the format, and nobody picks a
version number by hand. See [docs/RELEASING.md](docs/RELEASING.md).

**New here?** Read the [security model](docs/SECURITY.md) and the relevant
[ADRs](docs/SUMMARY.md#design-decisions-adr) before changing structure; the engine
contract itself is the `fmf-contract` crate, not a document. Contributions are
Apache-2.0 and follow the [Code of Conduct](.github/CODE_OF_CONDUCT.md). For a
failure, start with `just doctor`, the F12 panel,
`%APPDATA%\find-my-files\logs\app.log`, and the rolling `engine.<date>.log` files
under `%ProgramData%\find-my-files\logs\`. The date in an engine log filename is
UTC; each log line's `ts` field carries the process-cached local UTC offset.

## Architecture

```
WinUI 3 app (C#, unprivileged) ──named pipe──▶  fmf-service (Rust, LocalSystem)
   └─ IEngineClient boundary                       └─ fmf-core: $MFT scan, USN tailing,
       ├─ PipeEngineClient (default)                    in-memory index, query engine
       └─ FfiEngineClient ──P/Invoke──▶  fmf_engine.dll (in-proc fallback, elevated)
```

The engine contract — status codes, opcodes, wire structs, limits, protocol and
service names — is the dependency-free `engine/crates/fmf-contract` crate. It is
machine-readable, radiates the C# bindings, and is pinned by golden and drift
tests (ADR-0018); there is no prose copy to consult. `docs/RESEARCH.md` holds the
verified technical groundwork (MFT/USN APIs, prior art, performance baselines).

Canonical design docs are published in the
[design book](https://p4suta.github.io/find-my-files/book/). Validate them and
internal Rust docs with `just doc`; implementation APIs are not a product surface.

## License

Apache-2.0
