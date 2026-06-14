# find-my-files

**Instant file-name search for Windows — a FOSS take on Everything, built with a Rust engine and a native WinUI 3 UI.**

[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/P4suta/find-my-files/badge)](https://scorecard.dev/viewer/?uri=github.com/P4suta/find-my-files)

> Status: early development (pre-MVP). Nothing usable yet.

**Project page:** [p4suta.github.io/find-my-files](https://p4suta.github.io/find-my-files/) — overview in [日本語](https://p4suta.github.io/find-my-files/) / [English](https://p4suta.github.io/find-my-files/en/)

## Why

[Everything](https://www.voidtools.com/) is brilliant freeware, but it is closed source, its UI predates
modern Windows (system-DPI only, partial dark theme), and its future rests on a single developer.
Every FOSS clone so far either gave up the NTFS/MFT speed advantage for cross-platform reach, or
stalled in the unglamorous 80% (USN journal tailing, path reconstruction, memory-lean indexing).

find-my-files goes the other way: **Windows-only, file names only, as fast as Everything, genuinely FOSS.**

- Initial index by reading the NTFS $MFT directly (~seconds per volume)
- Real-time updates from the USN change journal — no filesystem watchers, no rescans
- Multithreaded SIMD substring scan over an in-memory index (~100 MB per million files)
- Pre-sorted indices: sorting a million results by name/size/date is instant
- Native WinUI 3: Mica, consistent dark theme, Per-Monitor V2 DPI (no blur on mixed-DPI setups)

## What it deliberately does NOT do

Content/property indexing, tags, previews, FTP/HTTP servers, FAT/exFAT/network drives (initially).
Indexing file names only is *the* reason Everything is fast. Feature creep is a non-goal.

## Where did the admin prompt go?

Reading the NTFS Master File Table and USN journal requires elevated volume access — the same
constraint Everything has. find-my-files splits that privilege off into a small Windows service
(`fmf-engine`, LocalSystem with stripped privileges); the UI runs unprivileged and talks to it
over a locked-down named pipe (same-user only — see `docs/SECURITY.md` for the threat model).

```
just service-install   # register + harden (elevated, once)
just service-start
```

Without the service installed, the app offers to relaunch elevated and runs the engine
in-process instead (`--engine=inproc`, the original MVP mode).

By default, hidden/system files — and everything under hidden/system folders
($Recycle.Bin contents, `pagefile.sys`, `.git` internals…) — are excluded from
results. A toolbar toggle brings them back instantly (they stay indexed).

Known limitations: names with unpaired surrogates are searchable but displayed with
replacement characters.

## Building

Toolchain is pinned via [mise](https://mise.jdx.dev/) (`mise.toml`), tasks run via `just`:

```
mise install        # rust + dotnet toolchains
just setup          # toolchain + git hooks (lefthook)
just build          # engine (cargo, release)
just test           # engine unit tests
just service-dev    # run the engine service in the foreground (elevated)
just index C:       # index a volume from the CLI (elevated terminal required)
```

The WinUI 3 app lives in `app/` (from milestone M1 onward).

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

- **[Design docs](https://p4suta.github.io/find-my-files/book/)** — architecture, ADRs, research, and the security model (rendered from `docs/` with mdBook)
- **[API reference](https://p4suta.github.io/find-my-files/doc/fmf_core/)** — Rust crate docs (rustdoc)

Both rebuild on every push to `main`; build them locally with `just doc`.

## License

Apache-2.0
