# ADR-0026: `fmf` CLI gets first-class DevEx polish (still a developer tool)

Date: 2026-06-20 / Status: Adopted

## Decision

Invest in the `fmf` developer CLI's ergonomics without expanding its remit. The CLI stays a developer / diagnostic / measurement tool — the WinUI app remains the end-user product — but it gains the polish a contributor (and anyone driving the engine from a terminal) expects:

- **`--version`** on the clap surface (same `CARGO_PKG_VERSION` `diag` already prints).
- **Global presentation flags** `--color auto|always|never`, `-q/--quiet`, `--format human|json`, threaded as a small `Ctx` into the commands that need them (colour is written to anstream's global choice instead).
- **`FMF_E_*` exit codes.** The top-level handler maps fmf-core's typed errors to the shared `fmf_contract::codes` table — the *same* classification the FFI boundary uses — so a script can branch on `$LASTEXITCODE` (NOT_ADMIN=3, VOLUME=4, LOCKED=7, …); clap's usage exit code (2) is untouched.
- **TTY-aware colour** (anstream strips ANSI when redirected) over an anstyle style vocabulary in `cmd::term`, plus an `indicatif` spinner during the volume index build. Both go silent under `--quiet`/`--format json`/non-TTY.
- **Machine-readable output.** `--format json` emits a single document on stdout (NDJSON for `watch`) for the commands that have a JSON shape (diag/bench/watch); failures emit a JSON error envelope on stderr. Every payload carries a `format_version`.
- **Generated reference + completions.** `docs/cli.md` (committed) and the shell completions in `build/completions/` (PowerShell/bash/zsh/fish) are both rendered from the single clap command tree, exposed as `fmf_cli::command()`. The `codegen` example writes them; `--check` is a drift tripwire wired into `just check` and pre-push, exactly like the contract drift check (ADR-0018).

To let the example and the integration tests reuse the clap surface, `fmf-cli` becomes a `lib` + `bin`: the parser and dispatch move to `lib.rs` (`command()` / `run()`), and `main.rs` is a one-line entry point. This preserves the "clap surface + dispatch only; logic in `cmd/`" rule.

## Rationale

- The two audiences the project wants to serve — people *developing* find-my-files and people *using* it from a terminal — both meet the engine through this CLI. It was the least-polished surface (no completions, no `--version`, monochrome, every failure exit 1), so the DevEx return is highest here. The contributor tooling (just/xtask) was already brought up to standard in the #54–58 pass.
- **Reuse, don't redefine.** Exit codes and the JSON error `code` come from `fmf_contract::codes`, the machine-readable contract source; the CLI mirrors the FFI's per-call classification rather than inventing a parallel table.
- **No new seams or ports** (ADR-0018): nothing here touches the engine's trait seams. Generated artefacts land under `build/` (ADR-0021); the one committed generated file, `docs/cli.md`, follows the same generate-commit-drift-check discipline as `contract.md`.
- **anstream/anstyle** already ride in via clap, so colour costs almost no new dependency surface and handles `NO_COLOR`/redirection correctly without hand-rolled TTY logic.

## Rejected alternatives

- **A one-shot `fmf search "<pattern>"` command.** Tempting for the "use it from my workflow" audience, but it is a new product surface (the engine builds an in-process index or needs a Rust-side pipe client to the running service), not polish. The REPL (`index`) and the WinUI app already cover interactive search. Deferred, not designed.
- **An end-user TUI.** Out of scope — the CLI stays diagnostic; the product is the app.
- **Unifying fmf-core's error enums behind one `code()` method.** fmf-core has several typed error enums (MftError, EngineCreateError, EngineError, ParseError/CompileError, UsnError); the FFI classifies them per call site. The CLI downcasts the same way in one place (`cmd::exit`) rather than driving a cross-crate refactor of the engine for a presentation concern.

## Consequences

- `fmf-cli` is now lib + bin. Its unit tests run under the lib; `assert_cmd` behavioural tests cover the non-elevated surface (version/help/usage/`diag`/JSON error); volume/USN assertions stay behind `FMF_ADMIN_TESTS`.
- `docs/cli.md` is a committed generated file: edit the clap surface, run `just cli-gen`, and `just check` / pre-push fail on drift. Completions are an ignored build output, bundled at release time.
- The `--format json` payloads are a **versioned, not frozen** contract: additive fields keep `format_version`; a field changing meaning or being removed bumps it. Snapshot/behavioural tests pin the current shape.
- New dev/runtime dependencies (anstream, anstyle, indicatif; dev-only clap_complete, clap-markdown, assert_cmd, predicates) pass `cargo deny`.

## Re-examination triggers

- If scripts come to depend heavily on exit codes and a misclassification surfaces, promote a single `code()` accessor into fmf-core and have both the FFI and the CLI consume it.
- If a non-interactive, scriptable query against the running service is genuinely needed, revisit `fmf search` via a Rust pipe client (a new design, with its own ADR).
- If the JSON shapes churn, bump `format_version` and document the change; if consumers need stability guarantees, freeze a subset.

## Follow-up

[ADR-0039](0039-cli-devex-pass-2.md) is the second DevEx pass that makes this ADR's
aspirations real and closes its gaps — completions are now actually generated into
the release bundle (and a `fmf completions <shell>` subcommand prints them on demand),
the `docs/cli.md` drift check became a `cargo test` (CI-gated, not just pre-push),
`--format json` is honoured by every result-producing command, and `-v/--verbose` was
added. The remit is unchanged: `fmf` stays a developer/diagnostic tool.
