# ADR-0021: Consolidate build output into a single build/ tree

Date: 2026-06-14 / Status: Adopted

## Decision

Consolidate all build artifacts into a single `build/` tree at the repository root.

```
build/
├── engine/        # cargo target-dir for the engine workspace
├── xtask/         # cargo target-dir for the xtask workspace
├── app/           # C# bin output (FindMyFiles / FindMyFiles.Tests)
├── dist/FindMyFiles/   # publish bundle = zip root: launcher FindMyFiles.exe + README.txt + app/
│   └── app/            #   self-contained app + engine binaries (apphost, runtime DLLs)
├── package/       # release zip + SHA256SUMS.txt
├── sbom/          # CycloneDX SBOM (release.yml)
├── site/          # GitHub Pages assembly (landing + book + doc)
└── docs-book/     # mdBook output
```

Mechanism (all means that do not violate the prohibition rules):

- **Rust**: per-workspace `[build] target-dir` in `.cargo/config.toml` (`engine/.cargo` → `../build/engine`, `xtask/.cargo` → `../build/xtask`). Relative paths resolve against the `.cargo/` parent (confirmed empirically with `cargo metadata`). **A single config at the repository root is rejected** (both workspaces would share one target and break the ADR-0018 separation rule).
- **C# bin**: each csproj's `BaseOutputPath` (`..\..\build\app\<proj>\`).
- **dist/package/site**: `xtask/src/paths.rs` as the single source of truth (`build_root`/`dist_dir`/`package_dir`/`engine_release_dir`/`site_dir`).
- **mdBook**: `build.build-dir = ../build/docs-book` in `docs/book.toml`.

## Rationale

- Artifacts were scattered across `engine/target`, `xtask/target`, `app/**/bin`, root `dist/`, root zip, root SBOM, and `site/`, making them costly to track and clean. A single `build/` means "delete it and everything is gone" plus an effectively one-line `.gitignore`.
- The target-dir in `.cargo/config.toml` is not a toolchain pin, so it does not violate the rule against placing `rust-toolchain.toml`/`global.json` (avoiding double management with mise).

## Consequences

- **C# obj stays put** (`app/**/obj/`). Relocating obj requires `BaseIntermediateOutputPath` to take effect during pre-restore evaluation, which effectively requires `Directory.Build.props`, but CLAUDE.md prohibits that file (it silently shadows the analyzer injection of `winapp run`). obj is intermediate output and already gitignored, so there is no real harm.
- The dev-tree `fmf-service.exe` lookup (`ServiceSetup.cs` production + pipe/contract tests) follows `build/engine/release`.
- **Bundle internals**: the `dist/FindMyFiles/` root holds only the launcher + `README.txt`; the self-contained app and engine binaries publish into `dist/FindMyFiles/app/` (the .NET apphost must stay co-located with its runtime DLLs, so it cannot move to the root). The root `FindMyFiles.exe` is a tiny native launcher (the `fmf-launcher` crate) that spawns `app/FindMyFiles.exe` — so a downloaded/extracted zip has one obvious thing to run. `paths::app_dir()` is the single source for the subfolder; the app's own relative discovery (`AppPaths`, `ServiceSetup.LocateServiceExe`) is unchanged because everything it needs stays beside the apphost in `app/`.
- The test-tmp fallback default in `testutil.rs` is `build/engine` (because the config.toml target-dir does not set the `CARGO_TARGET_DIR` env var).
- CI (ci/release/pages) artifact, SBOM, package, and Pages paths are all updated to under `build/`. `site/` remains the committed landing source; assembly output goes to `build/site`.
- Tools that assumed the old `engine/target` etc. (rust-analyzer, etc.) follow because they respect config.toml (reload if needed).

## Re-examination triggers

- If demand to also remove C# obj from the root grows strong and the `winapp run` analyzer-injection mechanism changes to no longer depend on `Directory.Build.props` (re-evaluate whether to allow props).
