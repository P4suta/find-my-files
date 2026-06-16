# ADR-0022: OS/shell/UI boundaries must use testable seams + behavioral tests

Date: 2026-06-15 / Status: Adopted

## Decision

Code that touches the OS, shell, processes, file I/O, or UI events must go through an **injectable seam** (an interface, or an `internal` core with paths/dependencies passed as arguments), and must come with tests that verify its **behavior** via `dotnet test` / `cargo test`. Do not ship with only pure helpers or argument construction tested while "actual behavior is unverified."

Canonical patterns: `app/FindMyFiles/Engine/IEngineClient.cs` (Fake/Ffi/Pipe), `Services/IDispatcher.cs`, `Services/IProcessRunner.cs` / `Services/IRevealApi.cs`, the path-parameterized core of `Services/FileLog.cs`. On the engine side, `engine/crates/fmf-core/.../seams.rs` (SnapshotStore / JournalSource; the two-seam cap is ADR-0018).

## Rationale

- **"Open folder and select file" (reveal) was broken from day one**: the actual behavior of `ShellOps.Reveal` (`SHOpenFolderAndSelectItems`) was never tested; only the pure helper `BuildOpenStartInfo` was green, and CI kept passing. The tests did not guarantee quality.
- Root-cause type: if the runtime/OS boundary stays `static` + direct P/Invoke, behavior cannot be swapped with a fake and behavioral verification cannot be written. Argument/structure tests do not make "passes = not broken" hold.
- The C# coverage gate being `Threshold=15` (nominal only) also allowed unverified code to ship.

## Consequences

- New boundary code is required at review to have "seam + behavioral test" (construction-only tests are deemed insufficient).
- C# live UI automation assumes a PowerShell script (`ui-tests.ps1`), which is disabled by execution policy on this machine and not adopted by operating policy. Therefore **UI-adjacent logic is pushed into ViewModels / core and verified via `dotnet test`** (no dependence on live UI automation).
- Mutation testing is used to detect vacuous tests (those that pass even when broken): Rust = `just mutants` (cargo-mutants), C# = `just stryker` (Stryker.NET). Informational for now; gated incrementally.
- The C# coverage gate is raised incrementally from 15% (ratchet).

## Re-examination triggers

- If live UI automation becomes adoptable without PowerShell dependence (e.g., integrating FlaUI into `dotnet test`) → re-evaluate direct testing of UI flows.
- Signs that seam proliferation distorts the design (the engine side keeps the two-seam cap = ADR-0018).
