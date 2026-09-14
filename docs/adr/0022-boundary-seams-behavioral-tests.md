# ADR-0022: OS/shell/UI boundaries must use testable seams + behavioral tests

Date: 2026-06-15 / Status: Adopted, extended in place — live UI automation and
mutation testing were later promoted from supporting practice to required
gates, which strengthens this decision rather than changing it. Extended again
on 2026-09-14: a reviewed `ignore-methods` inventory is now an accepted way to
remove a C# mutant, because a scheduling-only mutant has no stable verdict to
record, and the property it was standing in for moved to two deterministic
source gates. Tool versions are pinned in `mise.toml` and the tool configs, not
here.

## Decision

Code that touches the OS, shell, processes, file I/O, or UI events must go through an **injectable seam** (an interface, or an `internal` core with paths/dependencies passed as arguments), and must come with tests that verify its **behavior** via `just test` / `just test-app`. Do not ship with only pure helpers or argument construction tested while "actual behavior is unverified."

Canonical patterns: `app/FindMyFiles/Engine/IEngineClient.cs` (Fake/Ffi/Pipe), `Services/IDispatcher.cs`, `Services/IProcessRunner.cs` / `Services/IRevealApi.cs`, the path-parameterized core of `Services/FileLog.cs`. On the engine side, `engine/crates/fmf-core/.../seams.rs` (SnapshotStore / JournalSource; the two-seam cap is ADR-0018).

## Rationale

- **"Open folder and select file" (reveal) was broken from day one**: the actual behavior of `ShellOps.Reveal` (`SHOpenFolderAndSelectItems`) was never tested; only the pure helper `BuildOpenStartInfo` was green, and CI kept passing. The tests did not guarantee quality.
- Root-cause type: if the runtime/OS boundary stays `static` + direct P/Invoke, behavior cannot be swapped with a fake and behavioral verification cannot be written. Argument/structure tests do not make "passes = not broken" hold.
- The C# coverage gate being `Threshold=15` (nominal only) also allowed unverified code to ship.

## Consequences

- New boundary code is required at review to have "seam + behavioral test" (construction-only tests are deemed insufficient).
- UI-adjacent logic stays in ViewModels/core for deterministic unit coverage;
  the published bundle is additionally driven end-to-end by live UI automation
  (`just ui-test`), which is a required release/CI gate. It complements, and
  never substitutes for, the ViewModel/core behavioral tests — an automated
  click cannot assert an invariant the ViewModel does not expose.
- Mutation testing is a required gate, not an advisory score, because it is the
  only mechanism that detects vacuous tests (those that pass even when the code
  is broken): Rust = `just mutants`, C# = `just stryker`, both = `just mutation`.
  xtask owns the fixed invocations and canonical report parsing. The Rust run
  is non-shuffled and must pass cargo-mutants' copied-tree baseline; C# first
  passes the ordinary locked unit suite and Stryker's own initial run.
- Policy is the exact canonical survivor identity, never a percentage.
  Reviewed equivalent survivors live beside each tool config in
  `engine/mutation-baseline.json` and
  `app/FindMyFiles.Tests/mutation-baseline.json`, with a specific rationale per
  accepted identity. The same files pin the exact sorted source-file inventory,
  so a glob/config wiring omission cannot pass vacuously. For C#, xtask also
  resolves every exact `mutate` entry and requires that set to equal the
  baseline before Stryker starts. Stryker's whole-project JSON is accepted
  only under a closed-world rule: outside-scope mutants must be `Ignored` for an
  exact reason that proves they never ran, and inside-scope `Ignored` is allowed
  only for its exact redundant nested-block optimization (`Block removal
  mutation` plus `Removed by block already covered filter`) or for the reviewed
  `ignore-methods` inventory (`Removed by method filter`, below). Those
  identities and all outside-scope status counts remain in `gate.json`; they are
  never silently treated as killed mutants.
- A reviewed `ignore-methods` entry is an accepted way to remove a mutant, and
  `ConfigureAwait` is the only one on the list. Its argument selects a
  *scheduling* policy, and a scheduling policy is not something an
  exact-identity gate can judge: the same `PipeConnection.cs` mutant survived
  locally and was killed in CI by a load-sensitive pipe test, so "record it as an
  accepted equivalent" and "write a test that kills it" were both unstable
  verdicts — and the test-side suppression added to stop the kill did not stop
  it either. A verdict that depends on machine load cannot be pinned in a
  baseline at all, so the mutant is removed structurally instead, which is the
  option Stryker documents using `ConfigureAwait` as its own example. The cost is
  measured rather than assumed: in a run narrowed to `PipeConnection.cs`,
  `ignore-methods` dropped eight mutants there, only four of which were
  `ConfigureAwait` arguments. The filter takes the whole enclosing invocation, so
  it also removed the statement mutants that delete `await …ConfigureAwait(…);`
  outright, and a boolean argument belonging to the awaited call itself. Because
  the list therefore decides how much of the program stops being tested at all,
  xtask pins it: `ignore-methods` must equal the reviewed inventory exactly
  before any Stryker run starts — a single `["*"]` would otherwise retire the C#
  gate with every check still reporting green — and `Removed by method filter`
  identities are recorded one by one rather than counted.
- Giving up the mutation verdict on `ConfigureAwait` does not give up the
  property. It moves to two deterministic gates that read the source instead of
  racing it: `.editorconfig` raises CA2007/MA0004 to `error` under
  `app/FindMyFiles/Engine/Transport/`, where every await runs off the dispatcher
  under `Task.Run` and must say so; and `xtask/tests/repo_invariants.rs` fails if
  `ConfigureAwait(false)` appears anywhere in `app/FindMyFiles/ViewModels/`,
  where it once threw `RPC_E_WRONG_THREAD` at runtime with every unit test green
  (ADR-0036). Both rules are deliberately narrow. The directories in between mix
  dispatcher-bound and background work inside single files, so neither rule is
  true there, and stretching one to cover them would only make it a rule nobody
  can trust.
- New survivors, disappeared accepted identities, and file-inventory drift all
  require review. Malformed/missing JSON, duplicate keys or identities, an
  unexpected exact report schema, a Stryker exit/report mismatch, Rust
  timeouts, and C# timeout/no-coverage/non-redundant-ignored/non-terminal
  outcomes fail.
- The weekly/on-demand workflow runs Rust and C# independently without
  `continue-on-error`. Stable release re-runs both gates on the exact immutable
  source commit in a secretless job, and signing cannot start until it passes.
- The C# coverage gate is raised incrementally from 15% (ratchet).

## Re-examination triggers

- If the `winapp ui` public-preview surface changes incompatibly, keep its
  pinned version until the release suite is migrated and green.
- Signs that seam proliferation distorts the design (the engine side keeps the two-seam cap = ADR-0018).
