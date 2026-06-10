# find-my-files

**Instant file-name search for Windows — a FOSS take on Everything, built with a Rust engine and a native WinUI 3 UI.**

> Status: early development (pre-MVP). Nothing usable yet.

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

## Why administrator rights?

Reading the NTFS Master File Table and USN journal requires elevated volume access — the same
constraint Everything has (it ships a privileged service). The MVP runs elevated; a split
privileged-service + unprivileged-UI architecture is planned for v2.

By default, hidden/system files — and everything under hidden/system folders
($Recycle.Bin contents, `pagefile.sys`, `.git` internals…) — are excluded from
results. A toolbar toggle brings them back instantly (they stay indexed).

Known MVP limitations: drag & drop from Explorer into the (elevated) window does not work;
names with unpaired surrogates are searchable but displayed with replacement characters.

## Building

Toolchain is pinned via [mise](https://mise.jdx.dev/) (`mise.toml`), tasks run via `just`:

```
mise install      # rust + dotnet toolchains
just setup        # toolchain + git hooks (lefthook)
just build        # engine (cargo, release)
just test         # engine unit tests
just index C:     # index a volume from the CLI (elevated terminal required)
```

The WinUI 3 app lives in `app/` (from milestone M1 onward).

## Architecture

```
WinUI 3 app (C#, elevated)  ──P/Invoke──▶  fmf_engine.dll (Rust)
   └─ IEngineClient boundary                  └─ fmf-core: $MFT scan, USN tailing,
      (swaps to a named-pipe client                in-memory index, query engine
       when v2 splits off a service)
```

See `docs/ARCHITECTURE.md` for the FFI contract and `docs/RESEARCH.md` for the verified
technical groundwork (MFT/USN APIs, prior art, performance baselines).

## License

Apache-2.0
