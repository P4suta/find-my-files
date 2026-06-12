# Contributing to find-my-files

Thanks for your interest! A few conventions keep this project fast and
maintainable.

## Setup

The toolchain is pinned with [mise](https://mise.jdx.dev/) and tasks run through
[just](https://github.com/casey/just):

```
mise install     # rust, dotnet, just (pinned in mise.toml)
just setup       # toolchain + git hooks (lefthook)
```

Do not install toolchains ad hoc — declare them in `mise.toml` and run
`mise install`.

## Development loop

```
just check         # fast type-check (no codegen)
just verify        # fmt-check + lint + test + test-app (what pre-push runs)
just contract-gen  # regenerate the C# bindings if you changed the contract
```

`just service-dev` runs the engine service in the foreground (elevated). The
WinUI app talks to it over a named pipe; without the service installed it falls
back to an elevated in-process engine (`--engine=inproc`).

## Commit & PR conventions

Releases are automated with
[release-please](https://github.com/googleapis/release-please), which reads
[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` → minor bump
- `fix:` / `perf:` → patch bump
- `feat!:` or a `BREAKING CHANGE:` footer → major bump
- `docs:`, `refactor:`, `test:`, `chore:`, `ci:`, `deps:` → no release on their own

We squash-merge, so **the PR title must be a conventional commit** — that is the
line release-please sees.

## Before you push

- `just verify` must be green.
- Touched `fmf-core`? Run `just perf-gate` in an elevated, cool-machine shell
  (the perf discipline in `docs/adr/0013`).
- Never hand-edit `app/FindMyFiles/Engine/Generated/` — it is generated.

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
