# Contributing to find-my-files

Thanks for your interest! A few conventions keep this project fast and
maintainable. Please follow our [Code of Conduct](.github/CODE_OF_CONDUCT.md).

For the project's fixed rules, deliberate non-goals, and the elevation / UI / perf
conventions, see the [Development guide](docs/DEVELOPMENT.md) — read it before
changing anything structural.

## Setup

The toolchain is pinned with [mise](https://mise.jdx.dev/) and tasks run through
[just](https://github.com/casey/just):

```
mise install     # rust, dotnet, just (pinned in mise.toml)
just setup       # toolchain + git hooks (lefthook)
just doctor      # verify your environment matches the pins
```

Do not install toolchains ad hoc — declare them in `mise.toml` and run
`mise install`. Installing
[cargo-binstall](https://github.com/cargo-bins/cargo-binstall) first lets the
`cargo:` tools (mdbook, cargo-deny, cargo-llvm-cov, cargo-machete) fetch
prebuilt binaries instead of compiling from source.

## Development loop

```
just check         # fast type-check + contract/CLI-doc drift tripwires
just verify        # fmt-check + lint + test + test-app (what pre-push runs)
just contract-gen  # regenerate the C# bindings if you changed the contract
just cli-gen       # regenerate docs/cli.md + shell completions if you changed the CLI surface
just doc           # build the design docs (mdBook) + rustdoc locally
```

The `fmf` developer CLI's reference (`docs/cli.md`) is generated from its clap
surface; `just check` fails if it drifts. `just cli-gen` also writes tab
completions to `build/completions/` (PowerShell/bash/zsh/fish).

`just service-dev` runs the engine service in the foreground (elevated). The
WinUI app talks to it over a named pipe; without the service installed it falls
back to an elevated in-process engine (`--engine=inproc`).

## Commit & PR conventions

We use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`,
`fix:`, `perf:`, `docs:`, `refactor:`, `test:`, `chore:`, `ci:`, `deps:`) and
squash-merge, so the PR title becomes the commit. This is **enforced**: a local
lefthook `commit-msg` hook (`committed`) checks each message, and a CI gate checks
the PR title. The format isn't cosmetic — it drives automated versioning.

Releases are **not** hand-cut. [release-please](https://github.com/googleapis/release-please)
reads the Conventional Commits on `main` and keeps a "Release PR" open that bumps
the version (Rust workspace + C# app), updates `CHANGELOG.md`, and — when you
merge it — cuts the `vX.Y.Z` tag that fires `release.yml`. **You never pick or
edit a version number.** `feat:` → minor, `fix:`/`perf:` → patch, a `!`/`BREAKING
CHANGE:` → major. See [docs/RELEASING.md](docs/RELEASING.md) and
[ADR-0035](docs/adr/0035-automated-versioning-with-release-please-and-build-channels.md).

`fmf --version` (and the app's F12 panel) report a channel-aware build identity:
`X.Y.Z-dev+g<sha>` for your local build, `X.Y.Z-nightly.<date>+g<sha>` for a
nightly, and a clean `X.Y.Z` for a stable release — so a hand-built binary is
never mistaken for an official one.

## Before you push

- `just verify` must be green.
- Touched `fmf-core`? Run `just perf-gate` in an elevated, cool-machine shell
  (the perf discipline in `docs/adr/0013`).
- Changed the `fmf` CLI surface? Run `just cli-gen` so `docs/cli.md` stays in sync.
- Never hand-edit `app/FindMyFiles/Engine/Generated/` or `docs/cli.md` — both are generated.

## CI vs. local toolchain

`ci.yml` runs on the GitHub-stable Rust toolchain and `dotnet 10.0.x` (to catch
upcoming-stable breakage early), while `release.yml` builds the shipped artifact
on the exact `mise.toml` pins for reproducibility. Both are intentional.

## Scope

**file-name search only.** See the "out of scope" list in the feature-request
template before proposing new capabilities, and read the relevant ADR in
`docs/adr/` before changing architecture.

## License

By contributing, you agree that your contributions are licensed under Apache-2.0.
