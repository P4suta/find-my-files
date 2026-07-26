# Architecture and FFI Canonical Contract

This file is the **canonical FFI contract** — both the engine (Rust) and UI (C#) follow it; change signatures here first, then both sides. Design judgment and rationale live in `docs/adr/`.

## Overall Structure

```
┌────────────────────────────────────────────────────┐
│ WinUI 3 app (C#/.NET, asInvoker)                     │
│   ViewModels ── IEngineClient (swap boundary)         │
│        ├─ PipeEngineClient (default: named pipe)      │
│        ├─ FfiEngineClient (--engine=inproc, elevated) │
│        └─ FakeEngineClient (--fake-engine)            │
└───────┬──────────────────────────┬─────────────────┘
        │ named pipe               │ C ABI (in-proc)
┌───────▼────────────────────┐ ┌──▼──────────────────┐
│ fmf-service (priv service,  │ │ fmf_engine.dll        │
│  LocalSystem, least-priv)   │ │  (fmf-ffi crate,      │
│  pipe server+SCM+flush     │ │   cdylib) conversion, │
│  wire def = fmf-proto rlib   │ │  handle mgmt,         │
│                            │ │  catch_unwind only    │
├────────────────────────────┴─┴─────────────────────┤
│ fmf-core (rlib): VolumeIndex / query /               │
│   mft scan (ntfs-reader) / usn tail / persist        │
└──────────────────────────────────────────────────────┘
```

**1 FFI function = 1 pipe opcode, event callback = pipe push notification**. The wire spec is canonical in the
"Pipe Protocol" section of this document (design judgment in [ADR-0016](adr/0016-service-split-named-pipe.md) /
[ADR-0017](adr/0017-service-security-model.md)).

## Module Map (1 file = 1 responsibility)

Narrative order = data-flow order (ingest: mft/scan→usn→index, search: query→engine, cross-cutting: diag/metrics).

```
fmf-contract/src/ machine-readable canonical contract (ADR-0018, zero deps, no logic): codes / opcodes
                 / events(EventKind) / options(SortKey/CaseMode/VolumeState+from_u32)
                 / pod(repr(C)+const layout pin) / volume(label 16B padded) / versions
                 / limits / counters(counter roster) / bin/gen-contract(EngineContract.g.cs
                 emitter) / tests/drift(generated-output match — always within `just test`)
fmf-core/src/
├─ mft.rs        $MFT record format (consumed by scan)
├─ scan/         mod(scan_volume+ScanStats) / volume_io(raw volume open+fixup)
│                / pipeline(16MiB×3 read-ahead+sequential degrade) / parse(rayon parallel+RecordArena)
│                / deferred(NameCache 128Ki+LazyRecordReader — degrade returned via stats)
│                / probe(io-probe measurement; independent of main flow)
├─ usn/          records / apply / session(journal tailing)
├─ index/        mod(types+re-exports+in-place merge) / core(VolumeIndex+reads+derived caches)
│                / mutate(USN mutations) / snapshot(persistence; unsafe POD confined here)
│                / builder(2-pass build+EXCLUDED propagation) / compact(compaction) / frn
│                / testutil(TestDir RAII etc.; feature "testutil" for other crates' tests)
├─ query/        mod(AST/compile surface+wire→QueryOptions conversion) / exec(search driver+materialize)
│                / sweep(pool-sweep candidate gen) / matchers(residual eval) / memo(DirPaths/OffsetTable)
├─ engine/       mod(Engine+lifecycle+EngineEvent::to_wire=single point of event mapping)
│                / volume(VolumeSlot+install_index+checkpoint — home of state)
│                / worker(volume thread+pure transition-decision fns: snapshot_decision etc. — drives flow)
│                / seams(SnapshotStore+JournalSource, 2 traits only; no additional ports = ADR-0018)
│                / worker_tests(non-elevated deterministic replay of failure paths)
│                / search(cross-volume+k-way merge) / results(ResultSet+fill_page=single impl of
│                  row+blob build+STALE check) / tests
├─ diag.rs       init_diag(sole bootstrap for all entry points) / resolve_log_dir / error_chain(4KiB)
│                / degrade!(warn+counter, atomic) / diag ring+sink
├─ metrics.rs / wtf8.rs
fmf-ffi/src/     lib(contract re-export+export pin) / error / handle / events
                 / volumes / blob / results / contract_tests(literal absolute-value pin+ABI layout
                 +null/error paths — independent tripwire for canonical-source miss-edits). clippy.toml
                 forbids unwrap_or_default (compile-time rejection of silent swallow)
fmf-proto/src/   lib(contract re-export) / frame(16B header+length-prefixed codec)
                 / messages(payload codec — types in contract) / tests/golden(corpus pin)
fmf-service/src/ lib(module exposure — loopback tests drive the real server)
                 / pipe(overlapped I/O as Read/Write+listener; accept is a 2-wait on connect/stop Event)
                 / server(per connection: reader+2 workers+bounded request queue 2+
                 cancellation-aware backpressure+write mutex) / dispatch(opcode→Engine,
                 catch_unwind firewall, result-handle LRU64=evict is counter+warn) / events(Subscribe
                 +bounded queue 256) / config(service.json) / host(lock-loser 5s→60s retry)
                 / faults(--debug-faults: !!lag/!!panic/!!drop)
                 / security(SDDL build pin+SID capture+connect-time token check+dir DACL)
                 / svc(common serve core+SCM entry: Stop/PRESHUTDOWN→flush→graceful)
                 / main(run/install/uninstall --purge-data/start/stop/status/gc). clippy.toml same as above
fmf-cli/src/     main(clap defs+dispatch only) / cmd/{index,stats,bench,io_probe,criterion_gate,diag}
                 / bench_support(BENCH_QUERIES+baseline JSON shape+median+TempSnapshotGuard).
                 Developer build artifact only; never copied into the end-user bundle
app/FindMyFiles/
├─ Engine/       IEngineClient(boundary — interface+exception types only; CancellationToken on all async)
│                / EngineTypes(DTOs — synced with golden's actual shape) / EngineJson(sole definition of snake_case settings)
│                / Generated/EngineContract.g.cs(gen-contract generated; no hand-editing)
│                / EngineEventMarshaler(sole point of event→IDispatcher crossing)
│                / FakeEngineClient(contract-conformant: shares invalid_queries.json+BumpEpoch)
│                / PipeProtocol(codec — constants reference Generated) / PageCodec(row decode — same)
│                / NativeEngine(P/Invoke signatures+the other half of generated structs+startup SizeOf assert)
│                / EngineClientFactory(CLI>settings>auto selection)
│                / Transport/ PipeEngineClient(supervision+multiplexing only) / PipeConnection(ownership
│                  unit of a single connection — structural resolution of disconnect races) / PipeSearchResult / PipeServerIdentity
│                  / FfiEngineClient(callback guarded by generation counter)
├─ ViewModels/   MainViewModel(composition root) / SearchOrchestrator / ResultsPresenter
│                / NotificationCenter / PerfPanelViewModel / StatusFormatter / ResultRow
├─ Views/        PerfPanel(custom control for the F12 panel)
├─ Controls/     ResultsViewportManager(viewport save/restore, selection restore — UI thread only)
├─ Converters/   UiConverters(x:Bind static pure functions)
├─ Virtualization/ VirtualResultList(single lifetime+Reassign/epoch+per-epoch ct=double defense)
├─ Services/     IDispatcher(test seam) / DispatcherQueueDispatcher / Notifier / FileLog / ShellOps
│                / ExceptionPolicy(3 handlers+single home of crash marker)
│                / AppSettings(%APPDATA%\settings.json: engine mode etc.; corruption→warn+default+.bad save-aside)
└─ FindMyFiles.Tests/  xUnit(ManualDispatcher fake deterministically mimics the UI thread)
                 / Contract/(EngineClientContractTests abstract suite×4 derivations
                   + GoldenCorpusTests=identical byte pin across both languages)
```

Default visibility for new fields/methods is "within that responsibility's directory" (`pub(super)`). Exposure outside the crate is only via `pub use` in mod.rs.

## Engine Internals Key Points

Only the current structure is described here. For decision rationale, measured evidence, and rejected alternatives, see `docs/adr/`.

- **VolumeIndex (per volume, struct-of-arrays)**: folded names are deduplicated into the FMFIDX08 gapless dictionary `dict_pool`+`dict_off`; entries carry a `name_id`, and a name's length is the gap to the next offset ([ADR-0032](adr/0032-name-dictionary-encoding.md), [ADR-0033](adr/0033-phase3-memory-latency-levers.md)). The original is kept only on a fold mismatch via `orig_pool`+`orig_off` (`u32::MAX`=identical to fold; [ADR-0004](adr/0004-fold-overflow-name-layout.md)). Fold is length-preserving ([ADR-0003](adr/0003-wtf8-length-preserving-fold.md)). Size is a u32 column+overflow map ([ADR-0007](adr/0007-size-u32-overflow.md)). Each EntryId is one searchable directory link; hard-linked paths share a full FRN. FRN→EntryId is therefore a one-to-many equal-key range in a sorted id permutation ([ADR-0005](adr/0005-frn-index-sorted-permutation.md)). The only always-maintained sort permutation is name; size/mtime order is lazily derived ([ADR-0006](adr/0006-lazy-sort-permutations.md)). Path strings are not retained but lazily built via the parent chain. Deletions are tombstoned; compaction runs above a threshold.
- **Maintaining sort structure on USN batches**: binary search for the insertion point+in-place segment move (`index/mod.rs merge_sorted_tail`, [ADR-0008](adr/0008-insertion-point-batch-merge.md)).
- **Compaction**: the volume thread decides per batch apply. Sparse-row reclamation uses `len≥100k && tombstone>12.5%`; absolute pool garbage (`dead_name_bytes>32MiB`) and dictionary churn (`dict_appends_since_dedup>live_len/4`) bypass the row floor so small long-lived volumes cannot leak names indefinitely. An ascending old-id remap means the perm/FRN indexes need no re-sort ([ADR-0009](adr/0009-compaction-order-preserving-remap.md)). A copy is built under a read guard→`install_index` swaps it+structural bump→open result handles become hard STALE. Children of a dead dir go to root (push_raw's orphan policy), followed by derived EXCLUDED recomputation.
- **FRN index lookup semantics**: the compact permutation is keyed by low-48-bit record number: unmerged tail (newest first)→binary search, always tombstone-survivor filtered. Identity-sensitive USN operations then compare the complete 64-bit FRN, including sequence, so a delayed event cannot mutate a recycled record's new generation. The initial scan defers parent resolution to the parallel pass in `finish()`.
- **Hard-link convergence**: one EntryId is one searchable link; initial scan emits every searchable non-DOS `$FILE_NAME` and deduplicates exact `(full FRN,parent,name)` identities. `HARD_LINK_CHANGE` reads the complete current set. Because USN reasons accumulate, even `FILE_DELETE|HARD_LINK_CHANGE` removes all rows only after an exact `Gone` result. Every required link snapshot is preflighted before mutation; malformed/incomplete/I/O-failed metadata or an ambiguous multi-link rename rejects the whole journal batch, leaves its checkpoint unpublished, increments `hard_link_refresh_failures`, and forces a clean full rescan.
- **Default exclusion (EXCLUDED)**: raw H/S attributes+a computed EXCLUDED bit (self or an ancestor is H|S). Queries skip these by default (lifted via `include_hidden_system`). Inheritance is propagated by one cycle-safe O(n) forest pass at scan finish and, after directory parent/H/S changes, once at the USN batch boundary. Snapshot restore recomputes this derived state so older stale snapshots self-heal; compaction recomputes after orphan reattachment.
- **2-layer generation**: `content_generation` increments per USN batch (existing result handles can keep reading). `structural_generation` increments only on compaction/full rescan (existing handles become hard STALE=`FMF_E_STALE`). Replacement always goes through `VolumeSlot::install_index` (inheriting old+1; initial/snapshot restore does not bump). Not persisted in the snapshot (in-process monotonicity is enough).
- **Query-time materialize**: per volume, one-pass-filter the permutation→a sort-order-finalized contiguous array+multi-volume k-way merge (single volume is a direct copy). Subsequent page fetches are O(1) slices. A column click=re-issue with a different sort.
- **Incremental search (query cache)**: `VolumeSlot::last_query` holds the previous (compiled, options, both generations, ids). When the conservative subsumption rules in `query/subsume.rs` (same sort, single AND group, needle containment/range narrowing/filter addition only; fold bridging is orig→folded direction only) provably narrow the result, `query::refine` filters the previous ids via full evaluation — O(previous hit count). Correctness via oracle test (refine==fresh), kill switch `FMF_QUERY_CACHE=0`, observed in `QueryTrace.cache`.
- **Locking**: `parking_lot::RwLock`. Search=read, USN batch apply=write. The index has a single writer: one volume thread.
- **Threads**: initial scan=1 thread per volume. USN tailing=1 thread per volume (non-blocking read→drain→batch apply, parking ≤250ms on a quiet journal). Stop is cooperative: the worker re-checks its stop flag each park tick, so shutdown's join returns promptly even on an idle volume (no `CancelSynchronousIo`).
- **Initial scan**: $MFT is streamed in 16MiB chunks (1 read-ahead thread+3 buffers; startup failure degrades to sequential read+counter); within a chunk, rayon parses 1MiB subranges in parallel. Chunk-order append makes EntryId assignment deterministically match the sequential version (equivalence gate=admin test). Deferred ($ATTRIBUTE_LIST) base/extension records share a hard-bounded 128MiB arena; spills retain only record numbers and are reread lazily, with both spill/read failures mapped once by the worker ([ADR-0011](adr/0011-scan-streaming-pipeline.md)). File size comes only from the unnamed `$DATA` stream; named alternate data streams are intentionally not indexed.
- **Search execution**: query→AST→`CompiledTerm` sequence (cost order, AND short-circuit). rayon parallel over 64k chunks. The sweep is always on lower_pool. An uppercase needle / Sensitive does a superset sweep of the fold needle+original residual verification, resolving the fold-identical entry O(1) ([ADR-0004](adr/0004-fold-overflow-name-layout.md)). `dm:` is local TZ. No NFC/NFD normalization (known limitation). Trigram index not adopted ([ADR-0002](adr/0002-linear-sweep-no-trigram.md)).
- **Derived caches (DirPaths/SizePerm/MtimePerm)**: generation-managed per content_generation and extended incrementally from the previous generation where possible. DirPaths is lazily built on the first path query, with separate fold/orig slots, extended incrementally as long as the dir-topology generation is unchanged. Byte counts are charged to the B/entry gate via `IndexStats.derived_cache_bytes`.
- **Persistence**: `{index_dir}\{drive-letter}.fmfidx` (e.g. `c.fmfidx`), format **FMFIDX08** ([ADR-0010](adr/0010-snapshot-raw-pod-no-compat.md)); FMFIDX08 keeps the FMFIDX07 gapless columns but bumps semantic compatibility because older snapshots may contain only one representative row for a hard-linked object. temp→`MoveFileEx(REPLACE_EXISTING)`. On volume startup: load→verify→USN replay→live tail. Failure always falls back to a full rescan.

## FFI Contract (C ABI)

Common conventions:
- DLL name **`fmf_engine`**. All functions return an `int32_t` status (`FMF_OK=0`)+output args.
- Strings are UTF-8 (file names are **WTF-8**: invalid surrogates preserved; the C# side restores UTF-16 via a dedicated decode).
- Engine/result handles are opaque monotonic IDs transported as pointers.
  Engine-owned page/blob descriptors carry a separate monotonic `owner_id`.
  All functions are thread-safe. FFI re-entry from within a callback is forbidden.
- `catch_unwind` at every entry → `FMF_E_PANIC`. The detail message is in `fmf_last_error` (thread-local).
- **Pointer/length contract (caller's responsibility)**: at the C ABI boundary, Rust cannot validate array length or allocated capacity.
  - `(buf, cap)` output buffer (`fmf_list_volumes` / `fmf_index_status`): `buf` must point to **`cap`** writable `FmfVolumeStatus`. The engine writes at most `cap` entries and returns the true total in `*count` (`buf=NULL` is a size query that writes only `*count`).
  - `(volumes, n)` input array (`fmf_index_start`): `volumes` must point to **`n`** valid NUL-terminated UTF-8 `char*`.
  - POD pointers (`FmfQueryOptions*` / `FmfVolumeStatus*` / `FmfEvent*` …) must satisfy the declared `#[repr(C)]` size/alignment (C# marshals with the corresponding explicit layout, and `fmf-contract` pins it with compile-time `offset_of` assertions).
  - The engine null-checks every pointer and writes up to the `cap` limit, but **cannot detect a length claim exceeding the actual allocation** (undefined behavior). This contract is guaranteed by the sole caller, `FfiEngineClient`, constructing each array together with its length as a unit (this is why `fmf-ffi` uses `#![allow(clippy::missing_safety_doc)]` to delegate per-function safety notes to this section).

```c
// ── lifecycle ──
uint32_t fmf_abi_version(void);                         // currently 5; C# side checks at startup
// config_json: { "index_dir": "...", "log_dir": "...", "log_level": "info" } (required keys)
int32_t fmf_engine_create(const char* config_json, FmfEngineHandle* out);
int32_t fmf_engine_destroy(FmfEngineHandle h);          // joins internal threads+saves (explicit save is fmf_flush)

// ── events (fired from internal engine threads; receiver marshals to DispatcherQueue) ──
// kind: 1=Progress(volume, scanned) / 2=VolumeReady(volume, entries)
//       / 3=IndexChanged(200ms engine-side debounce, the only throttle)
//       / 4=RescanStarted(volume) / 5=VolumeFailed(volume) / 6=EngineError(severity)
typedef void (*FmfEventCb)(const FmfEvent* ev /*POD*/, void* user);
int32_t fmf_set_event_callback(FmfEngineHandle h, FmfEventCb cb, void* user); // cb=NULL to clear

// ── volumes and index ──
int32_t fmf_list_volumes(FmfEngineHandle h, FmfVolumeStatus* buf, uint32_t cap, uint32_t* count);
int32_t fmf_index_start(FmfEngineHandle h, const char* const* volumes, uint32_t n); // explicit start, async; current fixed-NTFS labels only
int32_t fmf_index_status(FmfEngineHandle h, FmfVolumeStatus* buf, uint32_t cap, uint32_t* count);
// FmfVolumeStatus.state: Scanning / Ready / Rescanning / Failed
// queries always succeed over "Ready volumes only" (UI judges the partial-result InfoBar by state)

// ── query (synchronous, fast; sort finalized at query time) ──
// options: { sort: Name|Size|Mtime, dir: Asc|Desc, case_mode: Smart|Insensitive|Sensitive,
//            include_hidden_system: bool (default false = exclude H/S attributes and their descendants),
//            regex_mode: u32 (bit0=interpret the whole query as one regex, bit1=scope 0=name/1=full path),
//            reserved: u32=0, presentation_basis: u64 (0=none; live result handle from this engine) }
int32_t fmf_query_control_create(FmfEngineHandle h, uint64_t* out_control_id);
int32_t fmf_query_control_cancel(uint64_t control_id); // idempotent while live
int32_t fmf_query_control_free(uint64_t control_id); // success preserves the preceding query error on this thread
int32_t fmf_query(FmfEngineHandle h, const char* query_utf8,
                  const FmfQueryOptions* options, uint64_t control_id,
                  FmfResultHandle* out, uint64_t* out_count,
                  FmfBlob** out_trace /* nullable: QueryTrace JSON */);

// ── observability (JSON blob; same engine-owned-ID pattern as FmfPage) ──
// FmfBlob { data: *const u8, len: u32, pad: u32=0, owner_id: u64 } — UTF-8 JSON
int32_t fmf_engine_stats(FmfEngineHandle h, FmfBlob** out); // MetricsSnapshot (recent trace, histograms, USN feed, per-column memory)
int32_t fmf_blob_free(uint64_t owner_id);
// ── page fetch: an engine-owned descriptor + row-header array + string blob. 1 P/Invoke, 1 copy ──
// FmfRow (56 bytes, no padding; fmf-ffi's contract_tests fix size/offset):
//   { entry_ref u64, frn u64, size u64, mtime i64,
//     name_off u32, parent_path_off u32, flags u32,
//     name_len u32, parent_path_len u32, reserved u32=0 } + trailing blob
// returns FMF_E_STALE = structural_generation mismatch. UI re-issues the same query
// FmfPage carries owner_id:u64 after its pointer/length fields.
int32_t fmf_result_page(FmfResultHandle r, uint64_t offset, uint32_t count, FmfPage** out);
int32_t fmf_page_free(uint64_t owner_id);
int32_t fmf_result_free(FmfResultHandle r);

// ── diagnostics ──
// len is in/out: in=buffer capacity, out=required/written bytes (excluding NUL).
// buf=NULL is the size probe. A non-NULL buffer must hold required+1 bytes;
// insufficient capacity returns FMF_E_INVALID_ARG and writes nothing.
int32_t fmf_last_error(char* buf, uint32_t* len);
```

Engine-owned pages and blobs stay in separate live-allocation registries keyed
by IDs from one process-wide monotonic `u64` namespace. The C# caller copies
`owner_id` from the descriptor before decoding and passes only that integer to
the matching free function; Rust never reconstructs ownership from a foreign
address. ID `0` is the no-allocation/free-no-op sentinel. Any unknown,
already-freed, forged, stale, or cross-kind nonzero ID returns
`FMF_E_INVALID_ARG`. Because IDs are never reused, an old descriptor cannot
free a newer allocation even if the allocator recycles the descriptor address
(ADR-0043). Result/engine handles follow the same reject-unknown monotonic-ID
rule.

Error code table (shared with the pipe protocol. **Append-only, no renumbering** — contract_tests pin the values): `FMF_OK=0, FMF_E_INVALID_ARG=1, FMF_E_STALE=2, FMF_E_NOT_ADMIN=3, FMF_E_VOLUME=4, FMF_E_QUERY_SYNTAX=5, FMF_E_IO=6, FMF_E_LOCKED=7, FMF_E_CANCELLED=8, FMF_E_PANIC=99`.
`FMF_E_LOCKED` = another process holds the index_dir writer lock (cross-process enforcement of the single-writer invariant; see the "Pipe Protocol" section).

```c
// ── explicit save (materialized in v2) ──
// Snapshot-saves only Ready volumes that are dirty (content_generation advanced since the last save).
// The service calls this internally on a schedule+at stop. Not exposed on the pipe.
int32_t fmf_flush(FmfEngineHandle h);
```

**Intentionally not included**: `fmf_entry_full_path` (unnecessary since a row carries name+parent_path).

## Pipe Protocol (versioned service split; current v4)

The wire spec between `fmf-service` (privileged service) and the non-privileged UI. This section is canonical. The machine-readable
definitions (error codes, opcodes, event kinds, POD, limits, version numbers) are held as the single canonical source by the
zero-dependency leaf crate **`fmf-contract`**, and `fmf-proto` (the encode/decode implementation),
`fmf-ffi`, and `fmf-service` radiate from it ([ADR-0018](adr/0018-contract-single-source.md);
the former claim "a cdylib cannot be depended on, so constants must be duplicated" was a factual error about Cargo — the only
impossible direction is depending **on** a cdylib). fmf-ffi's contract_tests remain as literal absolute-value pins,
serving as an independent tripwire that detects miss-edits of the canonical source itself.

### Transport

- pipe name: `\\.\pipe\fmf-engine-v4` (the protocol version is in the name; an incompatible change bumps the whole name. v4 adds cooperative query cancellation and a presentation-basis result ID; v3 widened both `FmfRow` WTF-8 lengths from `u16` to `u32`. ADR-0044/ADR-0042)
- byte mode (`PIPE_TYPE_BYTE`)+length-prefixed framing (message mode not used)
- creation flags: `FILE_FLAG_FIRST_PIPE_INSTANCE` on the **first instance only** (detects name pre-emption;
  the 2nd and later instances use the same SDDL with no flag — squatting is impossible as long as the server holds the first instance)
  + `PIPE_REJECT_REMOTE_CLIENTS` on all instances. Instance limit 8 (excess is connection-rejected+
  `pipe_connections_rejected` counter)
- DACL: explicit SDDL `D:P(A;;GA;;;SY)(A;;GRGW;;;<user SID>)` — only SYSTEM and the user SID captured at install.
  Authenticated Users not adopted (name leak on multi-user machines). Allowing Administrators also fails
  (a UAC-filtered token becomes deny-only, so the non-elevated UI cannot connect). As defense in depth,
  on connection accept the client token is checked against `authorized_sids` in `service.json`
  (`ImpersonateNamedPipeClient` reads the client SID)
- **The client opens the pipe at identification level** (C# `TokenImpersonationLevel.Identification` /
  Rust `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`). Left at the default anonymous level, the server's
  `ImpersonateNamedPipeClient` only gets an anonymous token, and the SID check above **rejects even an
  authorized user's connection** (`pipe client token rejected`). This trap is not exposed by console-mode tests
  that skip the check because `authorized_sids` is empty — it only shows up with an installed service
- client-side verification: for the default pipe name, `GetNamedPipeServerProcessId` → checked against the
  **PID of the SCM-registered fmf-engine service** (`QueryServiceStatusEx`) (anti-fake-server). Works in the non-elevated UI — a SYSTEM process's
  token cannot be opened non-elevated (ACCESS_DENIED), and the session 0 identity is unobtainable, so SYSTEM
  token checking cannot be used. A squatter cannot do SCM registration (admin required), so the PID will not match. When `--pipe-name`
  is specified (tests), verification is skipped

### Frame (16-byte LE header+payload)

```c
struct FrameHeader {            // 16 bytes, little-endian
    uint32_t len;               // payload length (excluding header). limit 16 MiB
    uint16_t opcode;            // see table below
    uint16_t flags;             // bit0=response, bit1=event push
    uint32_t request_id;        // request/response correlation. event push is 0
    int32_t  status;            // valid only on responses. error code table (shared with FFI)
};
```

- malformed frame (unknown opcode, operation-specific len overflow, truncation) = disconnect+`pipe_malformed_frames` counter+warn
- an error response (status != 0) carries UTF-8 detail in the payload (the mapping of `fmf_last_error` —
  thread-local pull does not exist on the pipe)
- requests are multiplexed by request_id (out-of-order completion allowed)

### Opcode table (correspondence to FFI functions)

Payload-notation legend: a type-annotated `{}` = **little-endian, no-padding POD byte sequence**.
"JSON" = UTF-8 JSON, **field names are snake_case (serde default)**. POD+variable-length data are concatenated
with no gaps in the listed order. The volume identifier is everywhere a **drive-label string `"C:"`** (GUIDs not used).
For both binary and JSON, the representative messages are pinned as identical **golden frames** (byte sequences)
in both the Rust and C# suites. The canonical corpus is **`contract/golden/`** (repository root): fmf-proto
`tests/golden.rs` and fmf-core `tests/golden_json.rs` capture and pin them, and on the C# side
`GoldenCorpusTests` independently decode→re-encode the same files and pin them. Re-capture is only an explicit
recaptured with `just contract-bless` (the explicit `FMF_BLESS=1` ritual for an intentional contract change — [ADR-0018](adr/0018-contract-single-source.md)).

| op | name | FFI mapping | payload (req → resp) |
|---|---|---|---|
| 1 | Hello | `fmf_abi_version` | `{protocol_version:u32}` → `{protocol_version:u32, abi_version:u32, server_pid:u32}` (protocol mismatch is INVALID_ARG+disconnect; ABI is diagnostic only on the pipe) |
| 2 | Subscribe | `fmf_set_event_callback(cb≠NULL)` | empty → empty. events pushed to this connection thereafter |
| 3 | Unsubscribe | `fmf_set_event_callback(NULL)` | empty → empty |
| 4 | ListVolumes | `fmf_list_volumes` | empty → JSON `[{"volume":"C:","state":0,"entries":0}]` (state equals FmfVolumeStatus.state) |
| 5 | IndexStart | `fmf_index_start` | JSON `{"volumes":["C:"]}` → empty. The whole request is validated before side effects: labels must match `[A-Za-z]:`, are canonicalized to uppercase, canonical duplicates are rejected, and every label must be in the current fixed-NTFS `ListVolumes` set. Any violation is synchronous `INVALID_ARG` with no worker or `VolumeFailed`; success idempotently ensures those volumes are indexed |
| 6 | IndexStatus | `fmf_index_status` | empty → JSON (same shape as ListVolumes) |
| 7 | Query | `fmf_query` | `FmfQueryOptions` (32B POD below)+UTF-8 query string (length derived from frame len, no NUL terminator) → `{result_id:u64, count:u64}`+QueryTrace JSON |
| 8 | ResultPage | `fmf_result_page` | `{result_id:u64, offset:u64, count:u32}` → `{row_count:u32, blob_len:u32}` → `FmfRow` (56B)× row_count (densely packed) → string blob (blob_len bytes, WTF-8). `name_off`/`parent_path_off` are byte offsets **relative to the start of the blob** (same layout as the FFI FmfPage) |
| 9 | ResultFree | `fmf_result_free` | `{result_id:u64}` → empty |
| 10 | Stats | `fmf_engine_stats` | empty → MetricsSnapshot JSON (same shape as FFI, snake_case) |
| 12 | ServiceInfo | (service-specific) | empty → JSON `{uptime_ms, connections, version}` |
| 13 | QueryCancel | `fmf_query_control_cancel` | empty one-way request using the Query request's `request_id`; no response |

Allocation-bearing request limits are contract, not tunables:
`Query UTF-8 bytes <= 4096`, parsed groups `<= 32`, parsed terms `<= 128`,
regex terms `<= 8`, `IndexStart JSON bytes <= 512`,
`IndexStart.volumes <= 26`, and `ResultPage.count <= 64`
(`fmf-contract::limits::{MAX_QUERY_BYTES,MAX_INDEX_START_PAYLOAD_LEN,MAX_VOLUMES,MAX_PAGE_ROWS}`).
The pipe reader applies the exact cap from the fixed header **before allocating
or reading the payload**: empty operations and unknown opcodes=0, Hello=4,
IndexStart=512, Query=32+4096, ResultPage=20, ResultFree=8, QueryCancel=0. Both FFI and pipe
boundaries reject larger semantic values with `FMF_E_INVALID_ARG`; any
operation-cap violation is a malformed frame and disconnects. The separate
16 MiB global cap exists for response frames, not as a request-allocation
allowance.

`FmfQueryOptions` (32B, alignment 8, LE — pinned by a contract test like FmfRow):
`{ sort:u32@0(0=Name 1=Size 2=Mtime), desc:u32@4(0=Asc 1=Desc),
case_mode:u32@8(0=Smart 1=Insensitive 2=Sensitive), include_hidden_system:u32@12(0/1),
regex_mode:u32@16(bit0=treat the whole query as one regex, bit1=scope 0=name/1=full path, high bits reserved 0),
reserved:u32@20(0), presentation_basis:u64@24(0=no basis; otherwise a live result ID owned by this connection/engine) }`

Mapping exceptions (C ABI specific, not present on the pipe): `fmf_engine_create`/`fmf_engine_destroy`
(absorbed into connection establish/disconnect and service lifetime), `fmf_page_free`/`fmf_blob_free` (ownership moves
to the client on frame receipt), `fmf_last_error` (inline detail in error responses).

### Event push

- To a Subscribed connection, push `flags=event, request_id=0, opcode=event kind` (equal to FFI kind 1–6) with the
  `FmfEvent`-equivalent POD `{kind:u32, _pad:u32, entries:u64, volume:[u8;16]}`.
  `volume` is a **UTF-8 drive label ("C:") 0x00-padded** (not a GUID)
- per connection a bounded queue (256)+a dedicated writer thread. When full, drop the oldest+`pipe_events_dropped`
  counter+warn — a slow/non-reading client never blocks the volume thread (never hangs).
  A dropped IndexChanged-class event self-heals on the next re-query
- because an event frame carries the event kind (1–6) in opcode, its number overlaps with request opcodes —
  **always discriminate first by the event bit in flags** (do not dispatch on opcode alone)
- the client's (re)connect sequence is fixed (this section is canonical): **Hello → Subscribe → IndexStatus →
  forced IndexChanged fire**. The last IndexChanged is **synthesized locally by the client**
  (the server does not send it) — to pick up, via re-query, changes missed while disconnected

### Result handle (result_id) lifetime

- the server holds `ResultSet`s in a per-connection registry. Freed by `ResultFree` or disconnect
- a nonzero `presentation_basis` must identify a live result owned by that same connection.
  `QueryTrace.unchanged=true` only when the new result has exactly the same ordered EntryRef sequence;
  stale, foreign, or freed IDs fail closed with `FMF_E_INVALID_ARG`
- limit 64/connection. On excess, **evict the least-recently-accessed (LRU)**, and a subsequent
  ResultPage for that result_id returns `FMF_E_STALE` (detail includes "evicted" to make it distinguishable from a structural generation change).
  the client recovers via the existing STALE→re-query path

### Single-writer exclusion (cross-process)

- `Engine::new` opens `{index_dir}\.writer.lock` with `FILE_SHARE_READ` only (write/delete sharing denied)
  and holds it for its lifetime. Failure is `FMF_E_LOCKED`. Read sharing keeps diagnostics possible without
  weakening the single-writer invariant; the lock auto-releases when the OS handle vanishes, so it never goes stale
- the service as the loser (in-proc UI got there first): backoff retry (5s→60s cap)+logs the holding process pid.
  Stops with an exit code that does not trigger an SCM failure-recovery (restart) loop
- the UI as the loser (`--engine=inproc` while the service is running): an explanatory InfoBar ("Service is running.
  To use in-proc, run `just service-stop`")

### Per-machine settings `%ProgramData%\find-my-files\service.json` (service-owned)

```json
{ "log_level": "info", "flush_interval_secs": 300, "idle_stop_secs": 300, "gc_max_idle_days": 7, "authorized_sids": ["S-1-5-21-…"] }
```

Unknown or obsolete keys are invalid; the installed service refuses to start instead of silently applying a partial configuration.
`log_level` is one of `trace|debug|info|warn|error`; `flush_interval_secs` is at least 10.
The file is capped at 16 KiB; `authorized_sids` contains at most the two distinct canonical user SIDs produced by installation.

- **On-demand lifecycle ([ADR-0027](adr/0027-on-demand-service-lifecycle.md))**: the service is `DEMAND_START` (not boot-resident). Install copies `fmf-service.exe` into the hardened data root, writes the exact `fmf-contract::versions::SERVICE_PROTOCOL_MARKER` into the SCM Description, and registers/points the GC task at that stable copy; the non-elevated UI reads that Description before starting a stopped service, so an old/missing marker routes straight to re-registration instead of waiting forever on a different versioned pipe. The service-object DACL lets authorized user SID(s) `start`/`stop` unelevated (start/stop/query only — never change-config/delete, which would be LPE on a LocalSystem service). The app starts it on launch (`StartThenPipe`); `serve()` self-stops after `idle_stop_secs` (default 300, `0` = stay resident) with no live connection; a daily SYSTEM Scheduled Task runs `fmf-service gc`, which uninstalls + purges when `last_use` is older than `gc_max_idle_days` (default 7, `0` = disabled). `uninstall --purge-data` removes the service, task, and data root; plain `uninstall` keeps only index/logs/config.
- **Elevation trust boundary**: `xtask publish` computes the SHA-256 of Windows'
  Authenticode PE digest stream for the exact bundled `fmf-service.exe` and
  embeds it in the managed app before the release signs both files. Immediately
  before `runas`, the UI holds no-write/no-delete handles on the image and each
  non-root parent, rejects reparse points and wrong file types, constant-time
  compares that embedded identity, and requires `WinVerifyTrust` success.
  Signing changes only the certificate table excluded by that digest. An
  unpinned ordinary developer build fails closed and cannot elevate an adjacent
  executable.
- **Elevated static imports ([ADR-0045](adr/0045-elevated-service-dependent-load-policy.md))**:
  the Rust payload statically links the MSVC CRT, and `fmf-service.exe` embeds
  `/DEPENDENTLOADFLAG:0x800` so its remaining static imports resolve from
  System32 only. `xtask publish` parses the source and copied PE Load Config;
  `xtask package` repeats the exact-value check independently.
- `fmf-service install` creates it together with capturing the user SID. Every start indexes all currently attached fixed NTFS volumes; `IndexStart` only discovers additions during a long-lived process.
  The initial default is all fixed NTFS volumes. **The non-elevated UI forwards its own SID via `--owner-sid`**, and install
  validates it with `validate_user_sid` (accepts only the real user type=SidTypeUser) before appending it to `authorized_sids`
  — because under OTS elevation (elevating with a different admin account) install's own SID differs from the everyday user's
- **`authorized_sids` is read exactly once at service start and baked into DACL construction and connect-time token checking
  (immutable while running)**. Reflecting an added SID requires `fmf-service restart` (= stop→start) — an in-place
  `install` alone does not affect a running instance (it keeps rejecting with the old allow list). The app's
  "register/re-register the service" runs install→restart in sequence
- ownership is separated from the per-user `%APPDATA%\find-my-files\settings.json` (UI-owned)

## C# Side Contract

- `IEngineClient` (swap boundary): `SearchAsync(query, options) → SearchOutcome(ISearchResult, QueryTrace)` / `GetStatsAsync` / `ListVolumesAsync` / `StartIndexingAsync` / `GetStatusAsync` (**3 methods changed to return Task in v2** — a synchronous call across the pipe is a "never hang" violation on the UI thread) / `event IndexChanged` / `event VolumeUpdated` / `event EngineErrorOccurred` / `EngineConnectionState Connection { get; }` + `event ConnectionChanged` (InProc | Connecting | Connected | Reconnecting; Ffi/Fake are fixed to InProc). The 3 implementations Fake/FFI/Pipe follow the same interface.
- **Engine selection** (`EngineClientFactory`): CLI override > settings > auto. Default auto queries SCM first: only exact `SERVICE_STOPPED` may start; every other lifecycle state (including stop/pause pending) or unreadable state gets a bounded pipe probe and never falls through to in-proc. A stopped service starts unelevated only when its SCM Description exactly matches `SERVICE_PROTOCOL_MARKER`; an old/missing marker routes to re-registration. A definitively absent service skips the pipe timeout and uses elevated FFI or the empty setup state. An explicit custom pipe is probed directly. Data-bearing Fake is development-only.
- **Disconnect and reconnect** (`PipeEngineClient`): disconnect = fail in-flight requests immediately with `EngineUnavailableException`, epoch-invalidate surviving `ISearchResult`s (afterwards `GetRangeAsync` → `StaleResultException` = the existing re-query mechanism is the recovery path), reconnect indefinitely with backoff (250ms→5s). The reconnect sequence is canonical in the "Pipe Protocol" section (`VolumeUpdated` events are synthesized and fired from the IndexStatus response). Requests have a default timeout of 10s.
- `SearchResultHandle : SafeHandle`. Page fetches bracket `DangerousAddRef/Release`, and do not release the underlying object even after `Dispose()` until in-flight fetches complete.
- page received→copy `owner_id`→copy to `ResultRow`→**immediately
  `fmf_page_free(owner_id)`**.
- the callback delegate is held in a client field (prevents GC reclamation). After receipt, to the UI via `DispatcherQueue.TryEnqueue`.
- **Search pipeline responsibility split** (MainViewModel is the composition root only):
  - `SearchOrchestrator` — when and what to search: 50ms debounce (clear is immediate), Dispose of stale results via the generation counter, `RequeryOrigin` classification, bounded Stale retry (1×), exception classification. **An empty query is not sent to the engine** (the product rule that an empty field has no results to return; a match-all enumeration would have its IDs shift every USN tick, so the start screen would redraw forever) — empty screen via `PresentEmpty` (idempotent). **During IME composition the query is held** (`TextCompositionStarted/Ended`; only the committed string flows through the normal debounce). **Focused mode** (focused search) = a pure query rewrite just before passing to the engine (`FocusedQueryRewriter`: add a `!path:` exclusion and one `ext:` whitelist item to each OR group; do not add ext to an explicit `ext:`/`regex:` group, nor an exclusion to a group containing `path:`/`\`) — does not touch the engine; settings in settings.json, ADR-0019.
  - `ResultsPresenter` — presenting results: prefetch the visible-range page **before** publishing, then publish atomically via `VirtualResultList.Reassign` (the old results stay on screen until the new ones are ready=zero blank frames). Count text and viewport placement events.
- two re-query families (`RequeryOrigin` classifies): **type/clear/sort/filter-originated=reset to top** / **IndexChanged/VolumeReady/Stale-originated=save the top visible index→restore, and selection restored best-effort only when an EntryRef in the seed matches**.
- `VirtualResultList` (non-generic IList+INCC+IItemsRangeInfo): **a single instance with the same lifetime as the page** (ItemsSource is x:Bind OneTime — replacing it discards the ListView virtualization state and causes flicker). New results are `Reassign(result, seeds)` = epoch++ → discard the page cache → apply seeds → **emit INCC Reset once** (UI thread only). **A re-query of the same result** passes the currently displayed live result as `presentation_basis`; the engine sets `QueryTrace.unchanged` only when the new ordered EntryRef sequence is exactly equal. That result is `RefreshInPlace` = epoch++ → swap the handle → in-place fill the visible seed into existing row instances (the MVVM setter notifies only on value change) → **no Reset, count text unchanged**. In-place updated size/mtime update only the cells whose value changed. The indexer never fetches and returns a placeholder (**out of range throws immediately** — no negative index, no fabricated fake page). On `RangesChanged`, background-fetch the visible range ±1 page in 64-row units→fill properties of existing ResultRows. Completion of an old-epoch fetch is silently discarded. Page LRU limit 4096 rows. Hard STALE receipt→`BecameStale` (only on epoch match)→ the Orchestrator re-queries.
- **IList contract invariant (do not falsely affirm membership)**: XAML blindly trusts the answers of `Contains`/`IndexOf`/`GetAt` via the WinRT adapter. A false "absent" is fixed by container re-realization, but a false "present" causes a crash deep in XAML at `GetAt(staleIndex)` (proven: the root of the `Int32.MaxValue-1` exception that reliably reproduced on search-with-results→clear-all). Membership is defined as "index is below Count AND the corresponding slot in the current page cache is that same instance". A row of an old result, a row of an LRU-evicted page, and a temporary row for enumeration always answer absent. Enumeration/CopyTo do not disturb the virtualization state (LRU). The UI-thread check of the mutation family (Reassign/RefreshInPlace) is always active in Release.

## Error Handling and Diagnostics (principle: "never crash, never hang, never go silent")

Every anomaly always reaches 3 paths: **(1) the log file (2) the diag ring (=auto-displayed in the F12 panel/fmf stats) (3) the UI InfoBar**. No telemetry is sent (local only); observability stays on-machine (OTLP/collectors are rejected — [ADR-0037](adr/0037-logfmt-diagnostics-and-correlation.md)).

- **Log format (both languages, [ADR-0037](adr/0037-logfmt-diagnostics-and-correlation.md))**: one **logfmt** line `ts level area [fields…] msg="…"` — human-readable *and* grep/awk-parseable. `ts` is RFC3339 + local offset; values are emitted bare unless they contain a space/`=`/`"`/control char, in which case they are quoted with `\r`/`\n`/`\uXXXX` escaping. That quoting is the **log-injection defence**. Values are capped at 1 KiB. The app's `FileLog` boundary additionally rejects exception messages/stacks and records only `error_type`/HRESULT/native error code, because exception text routinely contains user paths. The engine logfmt event formatter lives in `fmf-core::diag` (`LogfmtFormat`); the app's in `LogfmtFormatter` behind Serilog (the only logging dependency).
- **Logs**: engine=`%ProgramData%\find-my-files\logs\engine.<date>.log` (daily rotation, `max_log_files` generations — 14 for the service, 7 for FFI/CLI; filter via the `FMF_LOG` env var), app=`%APPDATA%\find-my-files\logs\app.log` (5 generations, rolled at 5MB). Stable binaries never place mutable state beside the executable. The app reads `FMF_LOG` too (shared spelling) for its initial level
- **Cross-process correlation**: one user query ties `app.log` to `engine.log` via the existing wire/handle ids. On the **pipe** path the engine groups a request's lines under `qid` = the frame `request_id` (already client-generated and echoed); on **both** paths the per-query "query served" line carries `rid` = the result handle (resultId on the pipe, the boxed handle's address on the in-process FFI path), which the UI logs too — `rid` is the universal join key. The query *text* never crosses diagnostics: logs carry `qlen`, and `QueryTrace`/`recent_queries` serialize `query_length` instead of the query. This is an intentional JSON/golden contract change; the binary ABI/POD and pipe frame remain unchanged.
- **diag ring** (fmf-core::diag): holds the most recent 128 tracing events at WARN or above+panics (with backtrace). Always included in `MetricsSnapshot.recent_errors`
- **panic**: caught by a global hook→log+ring. The volume thread has a `catch_unwind` firewall, so even on panic the UI always receives `VolumeFailed` (no silent hang)
- **Event kind 6 `FMF_EVENT_ENGINE_ERROR`**: a POD notification that a diag event occurred (entries=severity 1=warn/2=error/3=panic). Detail text is pulled from the stats JSON (push notification+pull detail)
- **Degradation recording convention (ADR-0018)**: a degradation path uses `fmf_core::degrade!` (the only way to do tracing::warn!+counter increment **atomically**; `rg degrade!` = enumerates all degradation paths). The batch path inside scan is the sole exception, returning the degradation in a `ScanStats` field and mapping it to counters+warn in one place at the worker layer (do not scatter the macro across the hot path). The boundary crates (fmf-ffi / fmf-service) forbid `unwrap_or_default` via disallowed-methods in clippy.toml — a silent fallback is rejected at compile time
- **Canonical source of counter names**: `fmf-contract::counters::COUNTER_NAMES` (C#'s CountersData is generated by gen-contract, and fmf-core's golden test reconciles CountersSnapshot's serde keys with the roster — a missing addition is mechanically detected)
- **Degradation counters** (`MetricsSnapshot.counters`, shown in F12 if nonzero): stat_fetch_failures / usn_batches_truncated / snapshot_load_failures / snapshot_save_failures / corrupt_mft_records / journal_rescans / scan_pipeline_fallbacks (scan read-ahead I/O thread startup failure→degrade to sequential read) / lazy_perm_rebuild_fallbacks (lazy sort-permutation watermark mismatch→degrade to full rebuild) / compaction_aborts (generation mismatch during compaction→discard the copy. Detects a break of the single-writer invariant) / pipe_malformed_frames (malformed frame→disconnect) / pipe_events_dropped (event bounded-queue overflow→drop oldest) / pipe_connections_rejected (instance limit exceeded) / deferred_name_cache_overflow (extension-record name cache full→degrade to disk read) / deferred_name_read_failures (targeted deferred-name read failure→retry through the authoritative live metadata source) / pipe_results_evicted (LRU eviction of a result handle) / trace_serialize_failures (QueryTrace JSON-ification failure→respond with an empty trace) / hard_link_refresh_failures (required complete link set unavailable/invalid→reject the batch and force full rescan)
- **Single implementation of error detail**: `fmf_core::diag::error_chain` (joins all causes, **4KiB limit+"…" truncation**) — both FFI `fmf_last_error` and the pipe error-response payload use this
- **Single home of diagnostics init**: `fmf_core::diag::init_diag(log_dir, level, max_log_files)` (logging+panic hook+diag ring connect, idempotent) is called by all entry points: FFI / service / CLI. log_dir resolution is `resolve_log_dir`: **explicit specification (config/CLI) > a `logs` subdir of the engine's `index_dir`** (co-located with the index, so it shares the index's writable, non-machine-wide pollution domain) — there is no machine-wide fallback (`%ProgramData%` dirtied the machine for non-elevated callers and panicked when unwritable); the machine service still logs to `%ProgramData%\find-my-files\logs` by passing it explicitly. This priority is implemented in only this one place
- **C# convention**: fire-and-forget always uses `task.Forget(area)` (exception→app.log+InfoBar). Shell operations go through `ShellOps`. A global exception handler writes a crash marker and notifies on the next start
- **Diagnostics copy**: the F12 panel's "Copy diagnostics" is a privacy boundary: every string in stats JSON is fail-closed redacted, the app log tail keeps only timestamp/level/area plus a numeric-field allowlist, and paths are symbolic (`%ProgramData%`/`%APPDATA%`). Numeric timing/counter/memory evidence remains useful without exporting file names, queries, exception bodies, or machine profile paths.

| FFI code | meaning | UI behavior | retry |
|---|---|---|---|
| FMF_E_QUERY_SYNTAX(5) | query syntax error | shown in the status bar | fix input |
| FMF_E_STALE(2) | structural generation change | auto re-issue the same query | automatic |
| FMF_E_NOT_ADMIN(3) | insufficient elevation | InfoBar+explanation | restart |
| FMF_E_LOCKED(7) | index_dir held by another engine | InfoBar+explanation ("Service is running. Use in-proc after just service-stop") | restart after stopping the service |
| FMF_E_PANIC(99) | panic inside the engine | InfoBar+pointer to engine.log | not possible (report) |
| others (1,4,6) | argument/volume/IO | InfoBar | depends |

## Latency Budget (breakdown of the change→on-screen ≤1s AC)

idle-edge USN discovery ≤250ms (the tail loop's non-blocking-read park; 0 on a busy volume, which never parks) + USN batch commit ≤100ms + engine IndexChanged debounce 200ms (the only event-rate throttle) + UI re-query ≤100ms + render ≤100ms = **≤750ms** worst case (≤500ms once the volume is active). Do not place an additional throttle on the UI side.

Additional budget for the pipe path (**canonical here** — other docs' numbers reference this section): ResultPage 64-row round trip p99
**≤5ms** (provisional — the loopback integration test asserts it, to be finalized by measurement). Continuously observed via F12's `PageRttEwma`.
Event push is one hop after the debounce above, so the budget structure does not change.

pipe test gates: protocol round-trip and loopback integration (unique pipe name+
`insert_ready_volume`) run unconditionally under non-elevated `just test`. The C# client × real fmf-service
integration is `FMF_PIPE_TESTS=1` (`just test-pipe`). The service E2E using real volumes is, as before,
`FMF_ADMIN_TESTS=1` (elevated); stable `release.yml` runs `just test-admin` on
the elevated Windows runner before the signing boundary.
