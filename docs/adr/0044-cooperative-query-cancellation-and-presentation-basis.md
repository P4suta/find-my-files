# ADR-0044: Cooperative query cancellation and explicit presentation basis

Date: 2026-07-26 / Status: Accepted.

Queries are cancellable end to end. `fmf-core` exposes a cloneable
`QueryCancellation` backed by an `AtomicBool`; it is checked at every phase
boundary and at bounded intervals inside sweeps, refinement, materialization,
derived-path construction, lazy sorting, and merge. Cancellation returns
`FMF_E_CANCELLED=8`, creates no result handle, records no served-query metric,
and never commits a partial per-volume refinement cache.

The pipe adds one-way opcode `QueryCancel=13`. Its payload is empty and its
frame `request_id` names the Query request to cancel. Each connection owns the
request-ID registry. A new Query cancels older queued and running queries
before it is queued (latest-query-wins); an explicit cancel is handled by the
reader without entering or waiting behind the work queue; disconnect cancels
every registered query. Normal Query completion still emits exactly one
response, including a cancelled response when cancellation wins.

The in-process ABI creates a monotonic opaque query-control ID before managed
code starts native work. Cancellation addresses that control ID; `fmf_query`
borrows its cancellation token, and the control is freed only after token
callback deregistration and query return. Unknown, forged, stale, reused, and
double-freed IDs fail closed. Cancellation itself is idempotent while the
control is live. This ordering eliminates the pre-cancel registration race.

`QueryTrace.unchanged` is no longer inferred from `VolumeSlot::last_query`.
That cache is only an internal refinement accelerator and may contain useful
work from another connection or a result that was never presented. A Query
instead carries an optional live presentation-basis result handle. Pipe and
FFI boundaries validate that the basis belongs to the same connection/engine
and is still live, then core compares the complete ordered ID column with the
new result. Only that exact comparison may set `unchanged=true`. A freed,
stale, cross-connection, cross-engine, cancelled, or missing basis behaves as
no basis. This makes the UI's `RefreshInPlace` decision an explicit
capability-based comparison rather than global ambient state.

The contract changes are intentionally incompatible:

- `ABI_VERSION` 4 → 5.
- `PROTOCOL_VERSION` 3 → 4 and pipe `fmf-engine-v3` →
  `fmf-engine-v4`.
- `FmfQueryOptions` grows from 20 to 32 bytes: the existing five `u32`
  fields, a required-zero `u32` reserved field, then
  `presentation_basis:u64`.
- status 8 is appended as `CANCELLED`; opcode 13 is appended as
  `QUERY_CANCEL`.

`HelloResp.abi_version` remains informational on the pipe. Named-pipe
compatibility and service probing require only an exact protocol version;
direct FFI loading continues to require an exact ABI version. The SCM
description marker therefore identifies the protocol and pipe only.

Rejected alternatives:

- **Managed cancellation only.** It hides stale UI work but leaves expensive
  native scans running and lets superseded work consume the bounded service
  queue.
- **A new core trait seam.** Cancellation is execution state, not an external
  dependency; adding a port would violate the two-seam architecture ceiling.
- **Global last-query identity.** It crosses connection/publication
  boundaries and can falsely authorize in-place refresh.
- **Thread interruption or killing a worker.** It cannot preserve Rust lock
  and allocation invariants. Cooperative checks provide deterministic cleanup
  with bounded latency.
