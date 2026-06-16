# ADR-0016: v2 service split — fmf-service + named pipe

Date: 2026-06-11 / Status: Accepted (only the duplicated contract constants + value-pin sync operation is superseded by [ADR-0018](0018-contract-single-source.md))

## Decision

Host the engine in a privileged service `fmf-service` (hosts fmf-core directly, LocalSystem), make the UI non-privileged (asInvoker), and connect over a named pipe. Wire definitions live in a new rlib `fmf-proto`, and `PipeEngineClient` becomes the third implementation of `IEngineClient`. The canonical spec is the "Pipe protocol" section of docs/ARCHITECTURE.md. The FFI (fmf_engine.dll) and in-proc paths persist for now (`--engine=inproc`, requires manual elevation).

## Rationale

- The MVP's requireAdministrator runs the whole app as administrator: UIPI kills Explorer→window drag & drop (known limitation in README), and "open" needed an explorer.exe de-elevation workaround
- The design reserved this split from the start: the fmf-ffi no-logic rule, the IEngineClient swap boundary, the per-machine index under %ProgramData%, and the "shared with the pipe protocol" note in the error-code table
- A resident service achieves "the index stays fresh via USN tracking even when the UI is not running"

### Rejected transports

- COM / RPC (out-of-process) — registry registration, marshalling definitions, and elevation-boundary complexity; worse wire observability vs. a length-prefixed named pipe
- gRPC / HTTP (localhost) — network stack drifts toward the "won't do" server features; dependency (tokio/tonic) clashes with fmf-core's synchronous threading; HTTP/2 overkill for local IPC
- Shared memory + events — fastest page transfer, but self-designing lifetime/permissions/generation loses the "1 FFI function = 1 message" mapping; unneeded since the pipe round-trip has budget headroom (baseline in ARCHITECTURE.md latency-budget section)
- async runtime (tokio) — at most a few connections; blocking I/O + threads fit the existing design; only adds dependency and build time

### flush exposure surface (3 options)

The premise is to materialize `Engine::flush()` (VolumeSlot's shared checkpoint + generation-pair dirty-skip). Three options for the exposure surface were compared:

- Option 1: expose as a pipe opcode — rejected. Client-driven flush spamming repeatedly holds index.read(), a local DoS path that stalls USN application (SECURITY.md threat 6)
- Option 2: not even in FFI, service-internal function only — rejected. The in-proc (--engine=inproc) path and tests cannot reproduce save timing, and it punches a hole in the contract mapping table (1 FFI function = 1 message)
- Option 3: adopted — FFI `fmf_flush` is exported, the pipe only reserves opcode 11 as a number

Saving is a service-internal responsibility — periodic (default 300s, staggered across volumes, dirty only) + on SCM Stop/PRESHUTDOWN. Because the PRESHUTDOWN default grace has been shortened to 10 seconds on current Windows (docs/RESEARCH.md), set an explicit extension via `SERVICE_PRESHUTDOWN_INFO` at install time.

### Distribution

MSIX/installer is deferred for this milestone (WindowsPackageType=None kept). Service deployment is established via `fmf-service install` (sc.exe cannot substitute, because SID capture, DACL setup, and privilege stripping must be done atomically) + a justfile recipe + README instructions. **Switching to asInvoker is conditioned on a working service-deployment mechanism** (default behavior when the service is not deployed: an InfoBar with explanation + fake fallback + a "Restart as administrator" button).

## Consequences

- 2 new crates (fmf-proto / fmf-service). fmf-ffi and the DLL name `fmf_engine` are unchanged
- The 3 synchronous IEngineClient methods (ListVolumes/StartIndexing/GetStatus) become Task-returning (sync across the pipe = a violation of the UI-thread "must not freeze" rule)
- The single-writer invariant extends across processes: `{index_dir}\.writer.lock` + `FMF_E_LOCKED=7`
- Both the Rust and C# test suites pin identical golden frames (byte sequences), fixing wire drift the same way as contract_tests
- Removal trigger for FfiEngineClient (--engine=inproc): completion of a one-release soak after service GA
- drag-out (results→Explorer) is filed separately as a new feature outside this milestone (only the drop direction is resolved here)

## Verification (measured 2026-06-11. Canonical numbers are the CLAUDE.md performance pass-line and the ARCHITECTURE.md latency-budget section)

- [x] First index, real C: **2.31s @1,268,560 entries** (gate: 1M≈60s. `just bench-check`). End-to-end via the real service binary (`service_admin.rs`, console-mode child process) also confirms real-C: scan→Ready→query
- [x] USN→event **250.9ms** (gate 1s. Measured with periodic flush at a 10s interval firing. Almost all of it is the intended engine-side 200ms debounce. The UI side adds the existing 50ms debounce + rendering)
- [x] kill→restart→restore **1.25s** (including process startup. Gate 2s. The engine-alone restore p50 is 108ms). The same test also proves the snapshot survives (durability) via the periodic flush before the hard kill
- [x] Search p99 **≤5.6ms** for all queries on real C: (gate 50ms) / the loopback round-trip for a 64-row ResultPage p99 **≤5ms** is constantly asserted by the test (`pipe_loopback.rs::page_roundtrip_stays_inside_the_latency_budget`)
- [x] RAM: the engine is the same code as the fmf-cli measurement (~99B/entry, WS 119.9MiB @1.27M). The fmf-service addition is only the pipe threads and queues (event queue cap 256×32B/connection)
- [ ] SCM registration (`fmf-service install` → start → stop → uninstall) real-machine smoke — **left as a manual procedure**. Registering the persistent LocalSystem auto-start service is done by user action (`just service-install`). The SCM-path code goes through the windows-service crate, and the serve core is shared with the console E2E
- [ ] SECURITY.md manual verification checklist (other-user reject / remote reject need a separate token / separate machine)

## Re-examination triggers

- If environments where pipe page-fetch p99 exceeds 5ms become routine (re-evaluate a multi-page batch-fetch opcode, or shared-memory page transfer)
- Real demand for concurrent multi-user use (`fmf-service authorize <user>` to register multiple authorization SIDs)
