# ADR-0039: CLI DevEx pass 2 — completions distribution, drift-in-CI, format consistency

Date: 2026-06-30 / Status: Accepted (no wire-contract / golden / ABI change; the `fmf` remit is unchanged — still a developer/diagnostic tool, [ADR-0026](0026-cli-surface-polish.md))

## Context

[ADR-0026](0026-cli-surface-polish.md) brought `fmf` to a first-class developer CLI (`--version`, `--color`/`-q`/`--format`, `FMF_E_*` exit codes, a versioned JSON envelope, generated completions + `docs/cli.md`). A second audit against industry-standard CLI ergonomics found gaps — some where the *documentation claimed a behaviour the implementation never had*:

1. **Completions were not actually distributed.** ADR-0026 and the `codegen` example both said completions were "bundled at release time", but neither `xtask publish` nor `package` copied `build/completions/` into the bundle — and there were no install instructions anywhere.
2. **The `docs/cli.md` drift check was not in CI.** It ran only as the `codegen --check` example via pre-push (`lefthook`), which `git push --no-verify` skips — unlike the contract drift check, which is a real `cargo test` gating CI. A stale reference could reach `main`.
3. **`--format json` was inconsistent.** `stats` ignored `--format` entirely (it always dumped pretty JSON, even in human mode, as *several* separate documents); `io-probe`, `spike` and `criterion-gate` did not even receive the format context, so `--format json` was silently ignored.
4. **Help was thin.** Positional `drive` arguments had no help on any command, there were no usage examples, `long_about` was unused, and `wrap_help` was off (long help did not wrap to the terminal).
5. **A confusing flag name.** `bench --json <path>` (write a report file) collided with the global `--format json` (stream to stdout).
6. **No CLI control of log level.** The level was hard-coded to `info`; verbosity could only be raised via the `FMF_LOG` env var.

## Decision

A second polish pass, entirely within ADR-0026's remit (no `fmf search`, no TUI, no new engine seams; the clap surface stays logic-free).

1. **Completions: distributed + on-demand subcommand.** A new `fmf completions <shell>` subcommand prints a completion script to stdout (the gh/rustup pattern: `eval "$(fmf completions bash)"`), rendered from the single `command()` tree. `clap_complete` moves from a dev-dependency to a normal one. `xtask publish` now ships `completions/{fmf.bash,_fmf,fmf.fish,_fmf.ps1}` by invoking the just-built `app/fmf.exe completions <shell>` — so the bundled scripts are produced by the exact binary they ship beside and cannot drift. Install steps are documented in the repo README and the bundled `README.txt`.
2. **`docs/cli.md` drift becomes a `cargo test`.** A new integration test (`tests/cli_docs_drift.rs`) re-renders the reference from `command()` and compares it (line-ending-normalised) to the committed file, so CI fails on drift — symmetric with the contract drift test. The pre-push `codegen --check` stays as a fast local tripwire.
3. **`--format json` everywhere.** Every result-producing command honours `--format`: `stats` emits one combined `format_version`-stamped document in json mode (human keeps the per-column dump); `io-probe`, `spike` and `criterion-gate` receive `Ctx` and emit JSON when asked. The interactive `index` REPL and `completions` are text-only by nature.
4. **Help quality.** `drive` help on every command, help on the remaining `io-probe` flags, a root `long_about` (stating this is a developer/diagnostic tool — the product is the WinUI app) and an `after_help` examples block, and the clap `wrap_help` feature.
5. **`bench --json <path>` → `bench --out <path>`**, removing the collision with the global `--format json`. (`just bench-baseline` updated.)
6. **`-v/--verbose`** (repeatable) maps to `info`/`debug`/`trace`; `FMF_LOG` still overrides it (`init_diag`).

These are additive to the JSON envelope (`format_version` unchanged); the clap-surface changes are reflected in a regenerated `docs/cli.md` (guarded by item 2).

## Rationale

- **Generate the bundled completions from the shipped binary**: any other source (the `codegen` example, a committed copy) could drift from the binary's real surface; `fmf.exe completions` cannot.
- **Drift as `cargo test`, not only pre-push**: pre-push is bypassable (`--no-verify`); the contract reference is already protected this way, and the CLI reference deserves the same.
- **`--format json` consistency over "not every command has JSON"**: a flag that is silently ignored is a worse experience than one that always means the same thing; the dev/measurement commands all have structured results worth emitting.
- **`completions` subcommand AND bundled files**: the subcommand is the portable, always-fresh path (and what power users expect); the bundled files mean a downloaded copy needs nothing built to install completions.

## Rejected alternatives

- **A `man` page (clap_mangen).** Rejected: find-my-files is Windows-only and ships no man reader, so a man page would have no consumer. Following the Unix convention here would add a build artdefact nobody can use — the project's dsa-first discipline says evaluate and decline, not follow blindly.
- **Sharing `cli_reference()` by promoting it into the library.** Rejected: it would pull `clap-markdown` into the shipped binary for a docs-only concern; the drift test re-implements the tiny render (≈3 lines) instead, and a divergence from the example surfaces as a loud test failure.
- **Erroring on `--format json` for commands without a JSON form.** Rejected in favour of actually giving every result-producing command a JSON form — the consistent, less surprising outcome.
- **Reviving `fmf search` / a TUI.** Out of scope here; still governed by ADR-0026's deferral (needs its own ADR + a pipe client).

## Re-examination triggers

- If a command's JSON shape needs to change meaning (not just add fields), bump `format_version` (ADR-0026).
- If completion scripts grow shell-specific install complexity, consider a `fmf completions --install` helper.
- If the CLI ever needs to be a scriptable end-user search surface, that remains an ADR-0026 question (`fmf search` via a pipe client, new ADR) — not a DevEx-polish change.
