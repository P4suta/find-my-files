# Development Guide

The shared, version-controlled handbook for working on find-my-files. It collects
the **invariants** a contributor must not break and points at the canonical source
for each one. When this guide and an ADR disagree, the ADR wins — open an issue so
this page can be corrected.

## Where to start (reading order)

1. **[CONTRIBUTING](https://github.com/P4suta/find-my-files/blob/main/CONTRIBUTING.md)** — setup, the development loop, commit/PR conventions.
2. **This guide** — the fixed rules and the project's deliberate non-goals.
3. **[ARCHITECTURE](ARCHITECTURE.md)** — the system design, the FFI/pipe contract, the threading model.
4. **[ADRs](adr/README.md)** — *why* each implementation was chosen, with numbers and re-examination triggers. Read the relevant ADR before changing structure.
5. **[RESEARCH](RESEARCH.md)** — verified external facts (MFT/USN APIs, prior art). Read before a design decision.
6. **[TROUBLESHOOTING](TROUBLESHOOTING.md)** — error codes, log locations, the in-app F12 panel.

## Scope — what we deliberately do NOT do

Filename-only indexing is *the* reason this engine is fast (content indexing makes RAM
balloon by an order of magnitude). Out of scope, and a non-goal:

> content search · property/tag indexing · preview · FTP/HTTP/ETP servers ·
> FAT/exFAT/network drives (initially) · ReFS (initially) · cross-platform.

Read the "out of scope" list in the
[feature-request template](https://github.com/P4suta/find-my-files/blob/main/.github/ISSUE_TEMPLATE/feature_request.yml)
before proposing a new capability. Feature creep is a non-goal — see
[ADR-0001](adr/0001-filename-only-index.md).

## Architecture fixed rules

These are the load-bearing invariants. Breaking one is a regression even if it compiles
and tests pass.

- **Dependency direction.** `app → IEngineClient → (PipeEngineClient → named pipe → fmf-service | FfiEngineClient → fmf_engine.dll (fmf-ffi)) → fmf-core`. Default is the pipe (non-privileged UI); `--engine=inproc` is the elevated in-proc fallback. The app touches the engine **only** through `app/FindMyFiles/Engine/IEngineClient.cs` (the Fake/Ffi/Pipe implementations share one mouth).
- **DLL name is `fmf_engine` — do not rename it.** The `[lib] name` in `fmf-ffi/Cargo.toml` and the C# `[LibraryImport("fmf_engine")]` are a matched pair ([ADR-0018](adr/0018-contract-single-source.md)).
- **No logic in `fmf-ffi`** (conversion, handle management, `catch_unwind` only). `fmf-service` dispatch is a mapping only — logic lives in `fmf-core`.
- **The contract has one source of truth.** Prose canon is [ARCHITECTURE](ARCHITECTURE.md) (the error table is **append-only, never renumbered**); the machine-readable canon is the dependency-free `fmf-contract` leaf crate (constants / repr types / layout / pure byte conversions — no logic). The codec is `fmf-proto`. Changes radiate one way: ARCHITECTURE.md → fmf-contract → `FMF_BLESS=1` re-capture → `just contract-gen` → both languages' tests green ([ADR-0018](adr/0018-contract-single-source.md)).
- **`app/FindMyFiles/Engine/Generated/` is generated — never hand-edit it.** Regenerate with `just contract-gen`; drift is caught by `cargo test` (and, after PR4, by `just check`).
- **Engine trait seams are capped at two:** `SnapshotStore` and `JournalSource` in `engine/seams.rs`. Adding more ports is disallowed ([ADR-0018](adr/0018-contract-single-source.md); re-examination trigger: a real regression on the admin-only path).
- **Pipe security:** [SECURITY](SECURITY.md) is canonical. SDDL is built **only** through the constructor functions in `fmf-service/src/security.rs` — never inline ([ADR-0017](adr/0017-service-security-model.md)).
- **Build output is consolidated under the repo-root `build/` tree** ([ADR-0021](adr/0021-build-output-layout.md)). The canonical path source is `xtask/src/paths.rs`. Only C# `obj/` stays under `app/**/obj/`. `.gitignore` excludes everything with the single line `build/`.

## Toolchain & shell conventions

- **Tools are pinned in `mise.toml`** (rust/dotnet/just + the `cargo:`/`github:` helpers). Do **not** install toolchains ad hoc with rustup/winget — add the tool to `mise.toml` and run `mise install`. There is deliberately no `rust-toolchain.toml` / `global.json` (it would double-manage with mise).
- **`just` is the single entry point** for dev/build work (`justfile` absorbs which shell runs). Don't call raw `cargo`/`dotnet`/`git` for routine tasks — add a recipe. Run `just doctor` after `just setup` to confirm your environment matches the pins.
- **Don't write shell-specific syntax in `just` recipes or `lefthook` hooks.** Pass env via `cargo --config 'env.X="1"'` or a dotnet `.runsettings`; express multiple steps as separate recipe lines / jobs, not `&&`. Ad-hoc one-shots are PowerShell (the primary shell); reach for Git Bash only when POSIX is genuinely required.
- **Procedural build/release/verification logic lives in the `xtask/` crate** (the cargo-xtask pattern); `just` is a thin wrapper. `xtask` is its own workspace at the repo root — never a member of the engine workspace. Pure logic in `xtask` gets unit tests; never inline PowerShell back into a recipe.
- Git hooks are managed by **lefthook** (`just setup` installs them). pre-commit: typos + rustfmt + taplo. pre-push: clippy + test + test-app (+ the xtask checks when `xtask/` changed). **Never bypass a hook** (`git push --no-verify` is forbidden) — the hooks are the quality gate.

## Editors

The repo ships editor config so the toolchain works out of the box:

- **VS Code** — open the folder. `.vscode/extensions.json` recommends rust-analyzer, CodeLLDB, the C# Dev Kit, Even Better TOML, and a justfile highlighter; `.vscode/settings.json` points rust-analyzer at **both** Cargo workspaces (engine + xtask — they are separate), runs clippy on save (matching the `-D warnings` gate), and folds generated `*.g.cs` under their source. `.vscode/tasks.json` exposes `check` / `test` / `verify` / `doctor` / `dev` as tasks; `.vscode/launch.json` debugs `fmf` / `fmf-service` via CodeLLDB (non-elevated targets — MFT/USN work needs an elevated editor).
- **Visual Studio / Rider** — open `FindMyFiles.slnx` (the app + its tests). The Rust engine is built through `just` / `cargo`, not the solution; `.editorconfig` carries the C# style and analyzer severities.
- **Any editor** — `rustfmt`, `taplo`, and `typos` are the canonical formatters/linters (`just fmt` / `just fmt-check`); `.editorconfig` covers the rest. Run `just doctor` to confirm your toolchain matches the pins.

## Elevation (administrator) rules

- Reading the `$MFT` and the USN journal **requires elevation** → that is the job of the `fmf-engine` service (or, from an elevated terminal, `--engine=inproc` / `just service-dev` / the real-volume tests and benches).
- The app (`asInvoker`) and the UI tests run **unprivileged**; they start with `--fake-engine` for deterministic data.
- `cargo build` / `cargo test` (unit + pipe loopback) / clippy / fmt are fine unprivileged — USN logic is tested by fixture replay.
- Elevation-required tests are gated by `#[ignore]` + `FMF_ADMIN_TESTS=1`; run them from an elevated shell with `cargo test -- --ignored` (or `just test-admin`).
- Don't start `--engine=inproc` while the service is running — the single-writer lock returns `FMF_E_LOCKED`. `just service-stop` first.

## UI fixed rules (WinUI 3)

Violating these reintroduces known regressions; see [ADR-0015](adr/0015-winui-data-virtualization.md).

- `ListView`'s `ItemsPanel` stays `ItemsStackPanel` (changing it kills virtualization).
- **Never swap `ItemsSource`.** `VirtualResultList` is a single page-lifetime instance; publish new results via `Reassign` (prefetched seed + Reset), except when the engine reports `QueryTrace.unchanged=true` — then `RefreshInPlace`, not Reset (avoids flicker on idle USN re-queries).
- No `ISupportIncrementalLoading` (crash, microsoft-ui-xaml#6883). Data virtualization = non-generic `IList` + `INotifyCollectionChanged` + `IItemsRangeInfo`. No `ItemsView`/`ItemsRepeater`.
- `x:Bind` + `x:Phase`; brushes via `ThemeResource` only (no hard-coded colors). Cache `DispatcherQueue.GetForCurrentThread()` on the UI thread; `TryEnqueue` from background.
- Hold FFI-callback delegates in a field (a collected delegate dangles native-side). "Open" goes through `explorer.exe "<path>"` (a direct open launches the associated app elevated). Don't add a custom `Directory.Build.props`.

## Performance bars

Verify with `just bench` before a release. The single-digit-millisecond feel is the
point — the documented numbers are ceilings, not targets. Discipline is in
[ADR-0013](adr/0013-measurement-discipline.md): compare cold, back-to-back, against a
real-volume absolute gate — never compare criterion runs taken at different times.

| Metric | Target (M2) |
|---|---|
| Initial index (real C:) | 250k ≈ 5s / 1M ≈ 60s |
| Query p99 (1M files, ≥3 chars) | ≤ 50ms |
| RAM (engine alone, bytes/file) | ≤ 110B |
| Change reflection (USN → UI) | ≤ 1s |
| Snapshot restore → ready | ≤ 2s |

Touched `fmf-core`? Run `just perf-gate` in an elevated, cool-machine shell before merging.

## Error-handling conventions (don't crash, don't hang, don't go silent)

- Logs: engine `%ProgramData%\find-my-files\logs\engine.log` (filter with `FMF_LOG`); app `%APPDATA%\find-my-files\logs\app.log`. Start incident triage there and in the F12 panel — see [TROUBLESHOOTING](TROUBLESHOOTING.md).
- C# fire-and-forget **must** use `task.Forget("area")` (`_ = SomeAsync()` is forbidden). Shell operations go through `ShellOps`.
- Rust degradation paths (those that recover via a fallback) **must** use `fmf_core::degrade!` (the one atomic way to warn + bump a counter; `rg degrade!` enumerates every degradation path). Exception: scan-internal batch paths return degradation via `ScanStats` and map to counters+warn in one place in the worker layer.
- Adding a counter is a three-part set: `metrics.rs` (Counters + CountersSnapshot) + `fmf-contract::counters::COUNTER_NAMES` + `just contract-gen`. Drift is caught by the golden test.

## Where state lives

- Index: `%ProgramData%\find-my-files\` (machine-wide; `.writer.lock` enforces a single writer across processes = `FMF_E_LOCKED`).
- Service config: `%ProgramData%\find-my-files\service.json` (owned by the service).
- UI settings: `%APPDATA%\find-my-files\settings.json`.
