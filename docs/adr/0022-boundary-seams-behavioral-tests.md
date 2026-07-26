# ADR-0022: OS/shell/UI boundaries must use testable seams + behavioral tests

Date: 2026-06-15 / Status: Adopted

> Current-state amendment (2026-07-25): live UI automation is now a required
> release/CI gate through `winapp ui`; it complements, rather than replaces,
> the ViewModel/core behavioral tests below.

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
  the published bundle is also driven end-to-end by `just ui-test`.
- Mutation testing is used to detect vacuous tests (those that pass even when broken): Rust = `just mutants` (cargo-mutants), C# = `just stryker` (Stryker.NET). Informational for now; gated incrementally.
- The C# coverage gate is raised incrementally from 15% (ratchet).

## Re-examination triggers

- If the `winapp ui` public-preview surface changes incompatibly, keep its
  pinned version until the release suite is migrated and green.
- Signs that seam proliferation distorts the design (the engine side keeps the two-seam cap = ADR-0018).
