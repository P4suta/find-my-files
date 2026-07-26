# Contributing to find-my-files

Thanks for your interest! A few conventions keep this project fast and
maintainable. Please follow our [Code of Conduct](.github/CODE_OF_CONDUCT.md).

Read the relevant [architecture](docs/ARCHITECTURE.md),
[security model](docs/SECURITY.md), [research](docs/RESEARCH.md), and
[ADR](docs/adr/README.md) before changing structure.

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
just check         # fast type-check + generated-contract drift tripwire
just verify        # fmt/lint + Rust/C# tests + dependency policy/usage gates
just contract-gen  # regenerate the C# bindings if you changed the contract
just doc           # build the design docs (mdBook) + rustdoc locally
```

The live `fmf --help` output is the developer CLI reference. `fmf completions
<shell>` emits development completion scripts; neither ships in the end-user
bundle.

`just service-dev` runs the engine service in the foreground (elevated). The
ordinary non-elevated app talks to that service over a named pipe and shows
setup when it is absent. `--engine=inproc` is an explicit elevated diagnostic
mode, not a non-elevated directory-scan fallback.

## Commit & PR conventions

We use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`,
`fix:`, `perf:`, `docs:`, `refactor:`, `test:`, `chore:`, `ci:`, `deps:`) and
squash-merge, so the PR title becomes the commit. This is **enforced**: a local
lefthook `commit-msg` hook (`committed`) checks each message, and a CI gate checks
the PR title. The format isn't cosmetic — it drives automated versioning.

Releases are **not** hand-cut. [release-please](https://github.com/googleapis/release-please)
reads the Conventional Commits on `main` and keeps a "Release PR" open that bumps
the version (Rust workspace + C# app), updates `CHANGELOG.md`, and — when you
merge it — creates the `vX.Y.Z` tag and draft Release, then dispatches the
exact-tag performance gate. Only after that gate succeeds does
`performance-release.yml` dispatch the publish-only `release.yml`. **You never
pick or edit a version number.** `feat:` → minor, `fix:`/`perf:` → patch, a
`!`/`BREAKING CHANGE:` → major. See
[the short release procedure](docs/RELEASING.md) and
[ADR-0035](docs/adr/0035-automated-versioning-with-release-please-and-build-channels.md).

`fmf --version` (and the app's F12 panel) report a channel-aware build identity:
`X.Y.Z-dev+g<sha>` for your local build, `X.Y.Z-nightly.<date>+g<sha>` for a
nightly, and a clean `X.Y.Z` for a stable release — so a hand-built binary is
never mistaken for an official one.

## Before you push

- `just verify` must be green.
- Touched `fmf-core`? Run `just perf-gate` in an elevated, cool-machine shell
  (the perf discipline in `docs/adr/0013`).
- Never hand-edit `app/FindMyFiles/Engine/Generated/`; use `just contract-gen`.

## CI vs. local toolchain

Ordinary CI and `release.yml` use the exact toolchain pins declared in
`mise.toml`. The shared Rust setup action reads that Rust pin directly; the C#
CI job mirrors the exact .NET SDK pin, while release jobs provision the manifest
through mise. The dated nightly Rust used by `fuzz.yml` is the intentional
sanitizer-only exception.

## Scope

**file-name search only.** See the "out of scope" list in the feature-request
template before proposing new capabilities, and read the relevant ADR in
`docs/adr/` before changing architecture.

## License

By contributing, you agree that your contributions are licensed under Apache-2.0.
