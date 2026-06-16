# ADR-0018: Contract single source of truth (fmf-contract) + capture-first golden corpus

Date: 2026-06-11 / Status: Adopted (supersedes only ADR-0016's "duplicate contract constants and sync by value pinning" practice.
The named-pipe adoption, rejected transport alternatives, flush public surface, and distribution decisions are unchanged)

## Decision

Introduce a **dependency-free leaf crate `fmf-contract` (rlib)** at the bottom of the dependency graph as the machine-readable source of truth for the engine contract (status codes, opcodes, event kinds, wire PODs, QueryOptions, limits, version numbers, pipe name), radiating a single definition to all Rust consumers (fmf-core / fmf-proto / fmf-ffi / fmf-service). For C#, the `gen-contract` binary inside `fmf-contract` radiates `app/FindMyFiles/Engine/Generated/EngineContract.g.cs` (constants, enums, `[StructLayout(Explicit)]` structs, CountersData DTO) as a **checked-in generated artifact**, and `fmf-contract/tests/drift.rs` (byte match between regeneration and the committed artifact) continuously detects drift inside `cargo test --workspace`.

The contract semantics are carried by **`contract/golden/`** at the repository root (manifest + byte streams + shared JSON fixtures) as an executable specification. The corpus is **captured from the current implementation before the refactor begins** (capture-first); thereafter both Rust (fmf-proto) and the independently hand-written C# codec (PipeProtocol/PageCodec) pin the same files. Re-capture (bless) only happens via explicit invocation with `FMF_BLESS=1` — the ritual for an intentional contract change; normal test runs require a match against the existing bytes.

Additionally, limit the engine's internal OS-effect seams to **`SnapshotStore` / `JournalSource` (2 traits only)** (to push the volume worker's failure paths down into non-elevated, deterministic tests), and forbid additional porting beyond this cap.

## Rationale

### The duplication rationale was based on a misreading of Cargo

The current `fmf-proto/src/lib.rs:3-5` and `fmf-ffi/Cargo.toml` claim that "fmf-ffi is a cdylib, so it cannot depend on / be depended upon, therefore the error-code table is duplicated and synced via value pinning in contract_tests." This is false: only the direction "another crate depends **on** a cdylib" is impossible; **a cdylib depending on an rlib is perfectly fine** (fmf-ffi already depends on fmf-core, an rlib). Placing a dependency-free rlib at the bottom replaces "duplicated definitions detected after the fact in tests" with "one definition that cannot drift," which structurally eliminates 3 of the 6 confirmed high-severity audit findings (duplicate code table, scattered event-kind magic numbers, unmet "pin the same golden bytes" claim).

### capture-first (corpus first, refactor second)

Generating the golden corpus "from the new contract crate" would bake generator bugs into the spec itself (self-consistency trap: a test that only proves the generator agrees with itself). Capturing and sealing the **current implementation's bytes** first means (1) "wire/ABI bytes unchanged" from S1 onward is proven by byte match rather than circumstantial evidence, and (2) the generator is put on the side required to "reproduce the captured bytes."

### Generation method: explicit command + check-in + drift test (not subject to ADR-0014)

`gen-contract` is not wired into MSBuild/build hooks (consistent with the no-custom-Directory.Build.props rule and ADR-0014's rejection of build complexity). Equivalent guarantees come from explicit `just contract-gen` invocation + committing the artifact + drift verification inside `cargo test --workspace` (rides on the existing lefthook pre-push / CI test job with no changes). FieldOffset and similar values are taken from the **`offset_of!` actual values of compiled Rust types**, so there is zero hand calculation and value drift is impossible in the type system. Missing enum entries are detected three ways: drift + golden + a C# startup `Marshal.SizeOf` assert.

### Rejected alternatives

- **Full Platform porting (~10 traits + new fmf-win crate)**: speculative generalization against the Windows-only charter; doubles I/O-seam maintenance permanently. Adopt only the 2 seams with demonstrated test value.
- **Wire version bump (pipe name v2 / event opcode cleanup / PROTOCOL_VERSION=2)**: contradicts the bytes-unchanged principle and ruins the captured corpus's regression-oracle property; the benefit does not justify the ritual cost. Do it in a separate ADR if needed.
- **Macro DSL for contract definitions (contract_consts! etc.)**: over-machinery for ~40 constants + 6 PODs; plain definitions + a `meta()` function (direct `offset_of!`) give the same guarantee.
- **A dedicated crate just for gen**: `fmf-contract/src/bin/gen-contract.rs` suffices and keeps the crate count at 6.
- **fmf-proto → fmf-core dependency (put the conversion layer on the core side)**: the contract source's leaf property would no longer be enforced by Cargo. Unify the dependency direction to core→contract and eliminate the conversion layer itself.
- **Wholesale vocabulary replacement (scan→ingest / diag→obs etc.)**: permanently diverges from the language of 17 historical ADRs and degrades the "read the relevant ADR before changing structure" workflow. Adopt only "narration order = flow order"; naming is unchanged.
- **Full state-machine rewrite of the volume worker**: rewriting concurrency invariants pinned only in prose (checkpoint-after-apply, compaction-generation recheck) cannot be proven old/new equivalent by new tests. Limit to behavior-preserving pure-function extraction + 2 seams.

## Impact

- 1 new crate (fmf-contract). **DLL name `fmf_engine`, pipe name `fmf-engine-v1`, ABI_VERSION=1, PROTOCOL_VERSION=1, FMFIDX04 are all bytes-unchanged** (no version bump).
- fmf-ffi's contract_tests is promoted from "duplicate equality pin" to "literal absolute-value pin + ABI layout pin" and lives on — an independent tripwire where a downstream test catches an accidental edit of the single source itself.
- Canonical contract-change flow (one-directional radiation): docs/ARCHITECTURE.md (prose) → fmf-contract (definitions) → `FMF_BLESS=1` re-capture → `just contract-gen` → both-language tests green. The error-code table remains append-only / no renumbering as before.
- C# decisions (user-confirmed): CountersData is also a generation target (counter additions auto-follow into C#); CancellationToken is fully propagated to `ISearchResult.GetRangeAsync` too (double defense with the epoch mechanism, fixed by a behavior test).
- Migration is 11 stages (S0→S0.5→S1a→S1b→S2 strict order; S3⇔S4, S5a/S5b⇔S4/S4b may run in parallel). Each stage compiles standalone + all tests green, mergeable to main. fmf-core-touching stages (S1b/S3/S4/S4b) require `just perf-gate` green in an elevated shell as a merge condition.

### S4 (scan.rs teardown) rollback clause

If the scan/ split exceeds the criterion 10% gate, **immediately roll back to file consolidation** before investigating the cause, and re-judge with ADR-0014's measurement procedure (same-time alternating A/B against the baseline-commit worktree). Because of `codegen-units=1`, module boundaries should be neutral to inlining, but measurement takes priority over hypothesis.

## Verification

- [ ] S0.5: capture corpus pinned by both Rust/C# suites on the same files (non-elevated `cargo test` +
  `just test-app`)
- [ ] S1a: after dependency inversion, corpus match proves wire bytes unchanged. All tests pass with C# unchanged (double proof)
- [ ] S2: byte match generated corpus == captured corpus (self-consistency trap closed) + drift test running
- [ ] S4: `streaming_scan_matches_reference` (elevated) + perf-gate green
- [ ] S4b: worker failure paths (snapshot corruption→rescan / journal-gone→Rescan→Ready / save failure) green
  in non-elevated, deterministic tests; old/new behavior identical in a real C: smoke
- [ ] S6: perf-gate + FMF_ADMIN_TESTS + FMF_PIPE_TESTS all green in an elevated shell, compared numerically
  against this appendix's starting point

## Re-examination triggers

- A real regression in an admin-only failure path that the 2 seams cannot cover (port addition gets its own ADR then)
- Contract-change frequency rises and the bless ritual becomes friction (re-evaluate build integration of generation)
- pipe page-fetch p99 > 5ms becomes the norm (inherits ADR-0016's re-examination trigger)

## Appendix: old→new path mapping (for history investigation; aids `git log --follow`)

| Old | New |
|---|---|
| fmf-proto `codes`/`PIPE_NAME`/`PROTOCOL_VERSION` | fmf-contract `codes`/`versions` (proto re-exports) |
| fmf-proto `QueryOptionsWire`/`WireRow`/`EventWire` | fmf-contract `pod::{FmfQueryOptions, FmfRow, FmfEvent}` |
| fmf-ffi `FMF_*` constants / POD definitions / `volume_bytes` | re-exported from fmf-contract / `volume::encode_label` |
| fmf-ffi `error_chain` / fmf-service/dispatch `error_chain` | fmf-core `diag::error_chain` (4KiB cap) |
| fmf-core `engine::VolumePhase` | fmf-contract `options::VolumeState` (name unified too) |
| fmf-core `scan.rs` (1165 lines) | `scan/{mod,volume_io,pipeline,parse,deferred,probe}.rs` |
| fmf-core `engine/volume.rs` thread body | `engine/worker.rs` (+`seams.rs`+`worker_tests.rs`) |
| fmf-cli `main.rs` (878 lines) | `main.rs` (135 lines)+`cmd/{index,stats,bench,io_probe,criterion_gate,diag}.rs`+`bench_support.rs` |
| C# `NativeEngine` struct / status constants | `Engine/Generated/EngineContract.g.cs` (generated, partial NativeEngine) |
| C# DTOs inside `IEngineClient.cs` | `Engine/EngineTypes.cs` (CountersData moves to the generated artifact) |
| C# connection / result handle inside `PipeEngineClient.cs` | `Engine/Transport/{PipeConnection,PipeSearchResult}.cs` |
| C# `MainPage.xaml.cs` (452 lines) viewport/perf/converter | `Controls/ResultsViewportManager` / `Views/PerfPanel` / `Converters/UiConverters` (181 lines remain) |
| C# `App.xaml.cs` 3 exception handlers | `Services/ExceptionPolicy.cs` |
| per-test unique tempdir duplication (%TEMP%) | fmf-core `index::testutil::TestDir` (build/engine/test-tmp, RAII) |

Stage commits: S0=9f7f4a6 / S0.5=c3916df / S1a=c9eb007 / S1b=fdb5407 / S2=7ce58e7 /
S3=6855336 / S4=4e99077 / S4b=261fbb7 / S5a=289e60a / S5b=540d79c / S6a=6226ea8 /
S6b=287f659+9d7a30d (+doc convergence commit).

## Appendix: starting-point record (at refactor start)

- Baseline commit: `97df250` (= feat/v2-service-split complete, ff-merged to main)
- Measured values (2026-06-11, from ADR-0016 verification section): first index real C: 2.31s @1,268,560 entries /
  USN→event 250.9ms / kill→restore 1.25s (restore p50 108ms) / search p99 ≤5.6ms /
  loopback ResultPage p99 ≤5ms / RAM ~99B/entry (WS 119.9MiB @1.27M)
- Non-elevated gate (`just verify`) green confirmed: run right after branch creation 2026-06-11 —
  fmt-check / clippy -D warnings / cargo test --workspace / C# 80/80 all pass

## Appendix: final gate judgment (2026-06-12, all stages complete)

- `FMF_ADMIN_TESTS=1` (elevated): green — `streaming_scan_matches_reference` (scan/ split
  equivalence gate), real C: E2E, USN live, service kill→restore all pass
- Real-volume absolute gate (`just bench-check`, elevated): **green, no regression** —
  1,289,867 entries 2.05s / search p99 all queries ≤6.2ms (gate 50ms) / restore p50 79ms (gate 2s)
- criterion 10% gate: 2 items exceeded initially → adjudicated with ADR-0013's alternating A/B/A:
  - `post_usn/apply_batch_1k` +10.6%: **noise**. Re-measuring identical code (97df250 itself vs its own
    baseline) gives CI −5.9%~+10.4% — this bench's intrinsic spread is about the same as the threshold.
    Re-measurement B was +2.5% (p=0.52)
  - `parse_compile` +13.7%→re-measure +5.7% (reproducible): **a real difference but accepted** — the absolute
    value is ~100ns/query out of 1.9µs (0.0002% of the p99 budget of 50ms). Probable cause: contract unification
    made SortKey/CaseMode repr(u32) (formerly rustc's default 1 byte), or a code-layout shift from changed
    declaration order. Since the source of truth (real-volume absolute gate) is green with wide margins on all
    items, it does not meet the trigger condition for the S4 rollback (file-consolidation rollback)
- criterion "committed" baseline re-recorded at the refactor tip (baseline for the next optimization session)
