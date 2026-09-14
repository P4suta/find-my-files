# Contributing

Thanks for looking. This file covers **how a change gets accepted**. It does not
restate how to build or test — `just --list` is the task menu and each recipe
carries its own description, and [README.md](README.md#building) has the setup
steps. Duplicating those here would just create a second copy to drift.

## Before you write code

**Check the change is in scope.** The project indexes file *names* and nothing
else, and that restriction is where the speed comes from. Permanently out of
scope: content search, property/tag indexes, file preview, FTP/HTTP/ETP serving,
FAT/exFAT/network drives, ReFS, and any non-Windows port. See
[ADR-0001](docs/adr/0001-filename-only-index.md).

**Structural changes need an ADR first.** Anything that moves a dependency
boundary, changes the wire contract, adds a seam, or alters the security model
is decided in [docs/adr/](docs/adr/) before it is implemented — including
decisions to *reject* something, which stay in the tree as `Rejected` or
`Superseded` rather than being deleted. The engine contract itself is the
`fmf-contract` crate, not a document; never write a prose copy of a constant,
offset, or layout.

**Security-relevant work:** read [docs/SECURITY.md](docs/SECURITY.md) first. It
is the canonical description of the named-pipe trust boundary and the service's
privilege model.

## Opening a pull request

- **Conventional Commits are required.** A `commit-msg` hook and a CI PR-title
  check both enforce the format, and release versions are derived from it — no
  one picks a version number by hand. See [docs/RELEASING.md](docs/RELEASING.md).
- **Run `just verify` before pushing.** The `pre-push` hook runs it for you.
  Never bypass hooks with `--no-verify`: they are the gate, not a formality.
- **If you touched `fmf-core`, run `just perf-gate`** from an elevated shell on
  a cool machine. The performance bar is a real acceptance criterion, not a
  guideline ([ADR-0013](docs/adr/0013-measurement-discipline.md)).
- **Tests describe behaviour, not implementation.** A test name should state the
  invariant it pins, so it keeps its value when the code underneath changes.
  Test doubles live behind the same seam as the real implementation.
- Fill in the pull-request template. Explain *why*, and say what you measured —
  claims about performance, memory, or coverage are expected to come with the
  numbers that back them.

## Quality gates

Every gate is executable, and a gate that cannot fail is treated as a bug.
Besides the usual build and test gates, this repository runs strict mutation
testing on a reviewed scope in both languages: the policy is the **exact set of
surviving mutants**, never a percentage, and a survivor is either killed by a
test or recorded with a written proof that no test could kill it
([ADR-0022](docs/adr/0022-boundary-seams-behavioral-tests.md)). Expect to do one
or the other; "add it to the baseline" on its own is not accepted.

## Reporting problems

- **Security vulnerabilities:** follow [SECURITY.md](.github/SECURITY.md). Do not
  open a public issue.
- **Bugs:** include the build identity (the About screen, or `fmf --version`),
  what you expected, and what happened. The F12 panel's **Copy diagnostics**
  button collects the useful state for you. Logs are at
  `%APPDATA%\find-my-files\logs\app.log` and
  `%ProgramData%\find-my-files\logs\engine.<date>.log`.
- **Environment problems:** run `just doctor` first — it checks the toolchain
  against the pins and reports drift.

## Licence and conduct

Contributions are licensed under Apache-2.0, the same as the project. All
participation is governed by the
[Code of Conduct](.github/CODE_OF_CONDUCT.md).
