# Contributing

This guide explains how contributors prepare and submit changes.

Use `just --list` to find build and test commands, and read the [Building guide](docs/README.md#building) for setup.

## Change scope

The project indexes file names only, and that restriction is central to its speed.

Content search, property indexes, tag indexes, and file previews are out of scope.

The product won't serve searches over `FTP`, `HTTP`, or `ETP`, or support non-Windows ports.

The project excludes `FAT` and `exFAT` volumes, network drives, and `ReFS`.

Read [ADR-0001](docs/adr/0001-filename-only-index.md) before proposing a scope change.

## Design and security

Record structural decisions in [docs/adr/](docs/adr/) before implementation when a change moves a dependency boundary, changes the wire contract, adds a seam, or alters the security model.

Keep rejected decisions in the ADR history as `Rejected` or `Superseded` rather than deleting them.

The `fmf-contract` crate is the sole source of wire constants, offsets, layouts, and pure byte conversions.

Don't copy those details into prose.

Read [docs/SECURITY.md](docs/SECURITY.md) before security-relevant work because it defines the named-pipe trust boundary and service privilege model.

## Changes and verification

Use Conventional Commits because the commit-message hook and CI pull-request title check enforce the format.

Release Please derives release versions and changelog entries from those commits.

Contributors must not choose version numbers manually.

See [docs/RELEASING.md](docs/RELEASING.md) for release details.

Run the full Windows `just verify` gate before merging.

Run the configured Git hooks and don't bypass them with `--no-verify`.

The pre-push hook invokes `just verify-push`.

On Windows, that task runs the full `just verify` gate, including native app tests.

Non-Windows hosts run `just verify-portable`, which doesn't replace the required Windows gate before merge.

When `fmf-core` or `fmf-runtime` changes, also run `just perf-gate` from an elevated shell on a cool machine before merging, as required by [ADR-0013](docs/adr/0013-measurement-discipline.md).

Write tests around observable behavior and keep test doubles behind the same seam as the real implementation.
Name each test after the invariant it checks.

Prefer types and executable checks for invariants.
Keep source comments for public contracts, safety requirements, legal notices, tool directives, and external constraints that the implementation can't express.
Write prose one sentence per line.

Complete the pull-request template with the reason for the change and measured evidence for performance, memory, or coverage claims.

Every gate must run an executable check and fail when its condition fails.
Treat a gate that can't fail as a bug.

[Verification scope](docs/verification.md) defines the production proofs, interpreter checks, preserved fuzz corpora, and native boundaries.
The applicable Linux lanes supplement the Windows source gate.

Strict mutation checks cover a reviewed scope in both Rust and C# and require accounting for the exact survivor set.

The policy uses the survivor set, not a percentage.

Detect each new survivor with a meaningful test or prove equivalent behavior for every admitted input.

A baseline entry alone is insufficient.
Follow [ADR-0022](docs/adr/0022-boundary-seams-behavioral-tests.md) for mutation checks.
Full mutation campaigns run at the Release gate.
Timeouts and incomplete runs supply no passing evidence.

## Reporting problems

Report security vulnerabilities through [.github/SECURITY.md](.github/SECURITY.md), and don't disclose them in a public issue.

For bugs, include the build identity from the About screen or `fmf --version`, the expected behavior, and the observed behavior.

Use the F12 panel's **Copy diagnostics** button to collect useful state.

Find app logs at `%APPDATA%\find-my-files\logs\app.log`.

Find engine logs at `%ProgramData%\find-my-files\logs\engine.<date>.log`.

For environment problems, run `just doctor` to compare installed tools with the repository's pinned versions.

## License and conduct

Contributors license their contributions under the project's Apache-2.0 license.

Follow the [Code of Conduct](.github/CODE_OF_CONDUCT.md).
