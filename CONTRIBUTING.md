# Contributing

Use `just --list` for the task menu and [README.md](README.md#building) for setup.
This document describes contribution requirements.

## Preparation

Check the scope before implementation.
The project indexes file names on Windows.
[ADR-0001](docs/adr/0001-filename-only-index.md) defines the supported scope and exclusions.

Structural changes need an ADR first.
Record changes to dependency boundaries, wire contracts, seams, and security in [docs/adr/](docs/adr/).
Keep rejected and superseded decisions.
The `fmf-contract` crate owns contract constants, offsets, and layouts.
Avoid prose copies.

Read [docs/SECURITY.md](docs/SECURITY.md) before security work.
It defines the named-pipe trust boundary and service privilege model.

Prefer types and executable checks for invariants.
Keep comments for public contracts, safety requirements, legal notices, tool directives, or external constraints that the implementation can't express.
Write prose one sentence per line.

## Opening a pull request

- Use Conventional Commits.
  The commit hook and PR-title check enforce the format.
  [Release Please](docs/RELEASING.md) derives release versions from those commits.
- Run `just verify` before pushing and keep hooks enabled.
- Run `just perf-gate` from an elevated shell on a cool machine after changing `fmf-core` or `fmf-runtime`.
  [ADR-0013](docs/adr/0013-measurement-discipline.md) defines measurement requirements.
- Test observable behaviour and invariants.
  Test doubles use the same seam as the real implementation.
- Fill in the pull-request template with the reason for the change and its validation.
  Support performance, memory, and coverage claims with measurements.

## Quality gates

Gates must reject representative broken behaviour.
[Verification scope](docs/verification.md) defines the production proofs, interpreter checks, preserved fuzz corpora, and native boundaries.
The applicable Linux lanes supplement the Windows source gate.

Mutation testing compares surviving changes with the reviewed baseline in each language.
Detect each new survivor with a meaningful test or prove that its behaviour matches the original for every admitted input.
Follow [ADR-0022](docs/adr/0022-boundary-seams-behavioral-tests.md).
Changing a threshold or accepting an unexplained survivor doesn't resolve a finding.

## Reporting problems

- Follow [SECURITY.md](.github/SECURITY.md) when reporting a vulnerability.
  Keep vulnerability reports private.
- Include the build identity, expected behaviour, and observed behaviour in bug reports.
  The About screen and `fmf --version` report the build identity.
  The F12 panel's **Copy diagnostics** button collects diagnostic context.
  Logs live at `%APPDATA%\find-my-files\logs\app.log` and `%ProgramData%\find-my-files\logs\engine.<date>.log`.
- Run `just doctor` when reporting an environment problem.

## Licence and conduct

Contributions use the project's Apache-2.0 licence.
Participation follows the [Code of Conduct](.github/CODE_OF_CONDUCT.md).
