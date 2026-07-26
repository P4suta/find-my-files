# ADR-0014: Build tooling rejection record and codegen-units=1

The historical cargo-nextest rejection is superseded by [ADR-0041](0041-nextest-bounded-test-execution.md). The rust-lld and sccache decisions remain active.

Date: 2026-06-11 / Status: Rejections recorded (codegen-units=1 accepted)

## Decision

Do not adopt rust-lld or sccache. The release profile keeps `codegen-units = 1` + `lto = "thin"` (engine/Cargo.toml). The historical nextest rejection below is retained as evidence but is superseded by ADR-0041.

## Rationale

- Fair A/B of rust-lld vs MSVC link.exe (3 crates in the engine workspace): fmf-cli incremental 1.72s vs 1.73s, full test link after fmf-core change 3.44s vs 3.46s — no difference. Zero measured improvement does not justify the risk of a non-standard linker (DLL output, CI divergence).
- sccache rejected because it disables incremental compilation. cargo-nextest was initially rejected because the then-small suite showed no benefit; ADR-0041 later adopted it for bounded execution and per-test attribution.
- codegen-units=1: rustc splits codegen units per module, so splitting the query kernel into exec/sweep/matchers/memo loses inlining and produces **~10% query latency** (A/B measured in the same machine state). With 1 unit, hot-path inlining is independent of module layout.

## Consequences

- Release build time grows by the codegen-units=1 amount (acceptable).
- The query kernel's file-split refactoring can be done independently of runtime performance.
- Build-speedup proposals should check this ADR first (re-proposal prevention).
- **rust-cache (Swatinem/rust-cache = GitHub Actions cache) is not a target of this ADR's rejection**: unlike sccache it does not wrap rustc invocations; it only archives/restores `~/.cargo` and target as artifacts, so it does not break incremental compilation. CI's `CARGO_INCREMENTAL=0` is also CI-workflow-only and does not propagate to local incremental. CI speedups (parallel job split, shared-key cache sharing, dll artifact sharing, PR cancel-in-progress) fall under this and are permitted (ci.yml).

## Re-examination triggers

- Re-measure rust-lld only if the workspace grows and link reaches the tens-of-seconds class.
