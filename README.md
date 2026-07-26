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
- Pre-sorted indices: sorting a million results by name/size/date is instant
- Native WinUI 3: Mica, consistent dark theme, Per-Monitor V2 DPI (no blur on mixed-DPI setups)

## What it deliberately does NOT do

Content/property indexing, tags, previews, FTP/HTTP servers, FAT/exFAT/network drives (initially).
Indexing file names only is *the* reason it's fast. Feature creep is a non-goal.

## Privilege model

Reading the NTFS Master File Table and USN journal requires elevated volume access.
The first-run button uses one UAC prompt to register a hardened, on-demand
`fmf-engine` service. After that the UI stays unprivileged and connects through
an authorized-user named pipe; it may start or stop only that service, with no
right to reconfigure or delete it. Explicit `--engine=inproc` remains an
elevated diagnostic fallback. See [the security model](docs/SECURITY.md).

By default, hidden/system files — and everything under hidden/system folders
($Recycle.Bin contents, `pagefile.sys`, `.git` internals…) — are excluded from
results. A setting brings them back instantly (they stay indexed).

## Building

Toolchain is pinned via [mise](https://mise.jdx.dev/) (`mise.toml`), tasks run via `just`:

```
mise install        # rust + dotnet toolchains
just setup          # toolchain + git hooks (lefthook)
just build          # engine (cargo, release)
just test           # engine nextest suite + Rust doctests
just service-dev    # run the engine service in the foreground (elevated)
just index C:       # index a volume from the CLI (elevated terminal required)
```

`fmf --help` is the developer CLI reference. Versions are channel-aware
(`dev`, `nightly`, `stable`).

Versioning and releases are automated from Conventional Commits — see
[docs/RELEASING.md](docs/RELEASING.md) (and the nightly build channel).

**New here?** Read [CONTRIBUTING](CONTRIBUTING.md), then the relevant
[architecture](docs/ARCHITECTURE.md), [security model](docs/SECURITY.md), and
[ADR](docs/adr/README.md) before changing structure. For a failure, start with
`just doctor`, the F12 panel, `%APPDATA%\find-my-files\logs\app.log`, and
`%ProgramData%\find-my-files\logs\engine.log`.

## Architecture

```
WinUI 3 app (C#, unprivileged) ──named pipe──▶  fmf-service (Rust, LocalSystem)
   └─ IEngineClient boundary                       └─ fmf-core: $MFT scan, USN tailing,
       ├─ PipeEngineClient (default)                    in-memory index, query engine
       └─ FfiEngineClient ──P/Invoke──▶  fmf_engine.dll (in-proc fallback, elevated)
```

See `docs/ARCHITECTURE.md` for the FFI contract and `docs/RESEARCH.md` for the verified
technical groundwork (MFT/USN APIs, prior art, performance baselines).

## Documentation

- **[Design docs](https://p4suta.github.io/find-my-files/book/)** — only the
  canonical architecture, security, research, release procedure, and ADRs

The design docs rebuild on every push to `main`; validate them and internal
Rust doc comments locally with `just doc`. Implementation APIs are deliberately
not published as a product surface.

## License

Apache-2.0
