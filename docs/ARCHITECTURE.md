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
                 emitter) / tests/drift(generated-output match — always within cargo test)
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
                 / server(per connection: reader+2 workers+write mutex) / dispatch(opcode→Engine,
                 catch_unwind firewall, result-handle LRU64=evict is counter+warn) / events(Subscribe
                 +bounded queue 256) / config(service.json) / host(lock-loser 5s→60s retry)
                 / faults(--debug-faults: !!lag/!!panic/!!drop)
                 / security(SDDL build pin+SID capture+connect-time token check+dir DACL)
                 / svc(common serve core+SCM entry: Stop/PRESHUTDOWN→flush→graceful)
                 / main(run/install/uninstall --purge-data/start/stop/status). clippy.toml same as above
fmf-cli/src/     main(clap defs+dispatch only) / cmd/{index,stats,bench,io_probe,criterion_gate,diag}
                 / bench_support(BENCH_QUERIES+baseline JSON shape+median+TempSnapshotGuard)
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

- **VolumeIndex (per volume, struct-of-arrays)**: names use the fold-overflow layout ([ADR-0004](adr/0004-fold-overflow-name-layout.md)) — the sweep target is the single folded `lower_pool`; the original is kept only on a mismatch via `orig_pool`+`orig_off` (`u32::MAX`=identical to fold). Fold is length-preserving ([ADR-0003](adr/0003-wtf8-length-preserving-fold.md)). Size is a u32 column+overflow map ([ADR-0007](adr/0007-size-u32-overflow.md)). FRN→EntryId is a sorted id permutation, keyed by indirection through the frn column ([ADR-0005](adr/0005-frn-index-sorted-permutation.md)). The only always-maintained sort permutation is name; size/mtime order is lazily derived ([ADR-0006](adr/0006-lazy-sort-permutations.md)). Path strings are not retained but lazily built via the parent chain. Deletions are tombstoned; compaction runs above a threshold.
- **Maintaining sort structure on USN batches**: binary search for the insertion point+in-place segment move (`index/mod.rs merge_sorted_tail`, [ADR-0008](adr/0008-insertion-point-batch-merge.md)).
- **Compaction**: the volume thread decides per batch apply (`len≥100k && (tombstone>12.5% || dead_name_bytes>32MiB)`). An ascending old-id remap means the perm/FRN indexes need no re-sort ([ADR-0009](adr/0009-compaction-order-preserving-remap.md)). A copy is built under a read guard→`install_index` swaps it+structural bump→open result handles become hard STALE. Children of a dead dir go to root (push_raw's orphan policy).
- **FRN index lookup semantics**: unmerged tail (newest first)→binary search. Always tombstone-survivor filtered (even with multiple pairs for the same key, at most one survives). The initial scan defers parent resolution to the parallel pass in `finish()`.
- **Default exclusion (EXCLUDED)**: raw H/S attributes+a computed EXCLUDED bit (self or an ancestor is H|S). Queries skip these by default (lifted via `include_hidden_system`). Inheritance is propagated O(n) at scan finish, and recomputed from the parent on USN insert/move. Limitation: a subtree move out of an excluded branch is stale until the next rescan.
- **2-layer generation**: `content_generation` increments per USN batch (existing result handles can keep reading). `structural_generation` increments only on compaction/full rescan (existing handles become hard STALE=`FMF_E_STALE`). Replacement always goes through `VolumeSlot::install_index` (inheriting old+1; initial/snapshot restore does not bump). Not persisted in the snapshot (in-process monotonicity is enough).
- **Query-time materialize**: per volume, one-pass-filter the permutation→a sort-order-finalized contiguous array+multi-volume k-way merge (single volume is a direct copy). Subsequent page fetches are O(1) slices. A column click=re-issue with a different sort.
- **Incremental search (query cache)**: `VolumeSlot::last_query` holds the previous (compiled, options, both generations, ids). When the conservative subsumption rules in `query/subsume.rs` (same sort, single AND group, needle containment/range narrowing/filter addition only; fold bridging is orig→folded direction only) provably narrow the result, `query::refine` filters the previous ids via full evaluation — O(previous hit count). Correctness via oracle test (refine==fresh), kill switch `FMF_QUERY_CACHE=0`, observed in `QueryTrace.cache`.
- **Locking**: `parking_lot::RwLock`. Search=read, USN batch apply=write. The index has a single writer: one volume thread.
- **Threads**: initial scan=1 thread per volume. USN tailing=1 thread per volume (blocking read→drain→batch apply). Stop via `CancelSynchronousIo`.
- **Initial scan**: $MFT is streamed in 16MiB chunks (1 read-ahead thread+3 buffers; startup failure degrades to sequential read+counter); within a chunk, rayon parses 1MiB subranges in parallel. Chunk-order append makes EntryId assignment deterministically match the sequential version (equivalence gate=admin test). Deferred ($ATTRIBUTE_LIST) names are resolved from a RAM cache of extension records ([ADR-0011](adr/0011-scan-streaming-pipeline.md)).
- **Search execution**: query→AST→`CompiledTerm` sequence (cost order, AND short-circuit). rayon parallel over 64k chunks. The sweep is always on lower_pool. An uppercase needle / Sensitive does a superset sweep of the fold needle+original residual verification, resolving the fold-identical entry O(1) ([ADR-0004](adr/0004-fold-overflow-name-layout.md)). `dm:` is local TZ. No NFC/NFD normalization (known limitation). Trigram index not adopted ([ADR-0002](adr/0002-linear-sweep-no-trigram.md)).
- **Derived caches (OffsetTable/DirPaths/SizePerm/MtimePerm)**: generation-managed per content_generation, extended incrementally from the previous generation where possible (OffsetTable fully rebuilds above a stale ratio of n/8; watermark mismatch→warn+counter+rebuild). DirPaths is lazily built on the first path query, with separate fold/orig slots, extended incrementally as long as the dir-topology generation is unchanged. Byte counts are charged to the B/entry gate via `IndexStats.derived_cache_bytes`.
- **Persistence**: `{index_dir}\{drive-letter}.fmfidx` (e.g. `c.fmfidx`), format FMFIDX04 ([ADR-0010](adr/0010-snapshot-raw-pod-no-compat.md)). temp→`MoveFileEx(REPLACE_EXISTING)`. On startup: load→verify→USN replay→live tail. Failure always falls back to a full rescan.

## FFI Contract (C ABI)

Common conventions:
- DLL name **`fmf_engine`**. All functions return an `int32_t` status (`FMF_OK=0`)+output args.
- Strings are UTF-8 (file names are **WTF-8**: invalid surrogates preserved; the C# side restores UTF-16 via a dedicated decode).
- Handles are opaque pointers. All functions are thread-safe. FFI re-entry from within a callback is forbidden.
- `catch_unwind` at every entry → `FMF_E_PANIC`. The detail message is in `fmf_last_error` (thread-local).
- **Pointer/length contract (caller's responsibility)**: at the C ABI boundary, Rust cannot validate array length or allocated capacity.
  - `(buf, cap)` output buffer (`fmf_list_volumes` / `fmf_index_status`): `buf` must point to **`cap`** writable `FmfVolumeStatus`. The engine writes at most `cap` entries and returns the true total in `*count` (`buf=NULL` is a size query that writes only `*count`).
  - `(volumes, n)` input array (`fmf_index_start`): `volumes` must point to **`n`** valid NUL-terminated UTF-8 `char*`.
  - `(roots, n, excludes, m)` input arrays (`fmf_index_start_scope`): `roots`/`excludes` must point to **`n`**/**`m`** valid NUL-terminated UTF-8 `char*` (scope mode, ADR-0024/-0025).
  - POD pointers (`FmfQueryOptions*` / `FmfVolumeStatus*` / `FmfEvent*` …) must satisfy the declared `#[repr(C)]` size/alignment (C# marshals with the corresponding explicit layout, and `fmf-contract` pins it with compile-time `offset_of` assertions).
  - The engine null-checks every pointer and writes up to the `cap` limit, but **cannot detect a length claim exceeding the actual allocation** (undefined behavior). This contract is guaranteed by the sole caller, `FfiEngineClient`, constructing each array together with its length as a unit (this is why `fmf-ffi` uses `#![allow(clippy::missing_safety_doc)]` to delegate per-function safety notes to this section).

```c
// ── lifecycle ──
uint32_t fmf_abi_version(void);                         // currently 1; C# side checks at startup
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
int32_t fmf_index_start(FmfEngineHandle h, const char* const* volumes, uint32_t n); // explicit start, async; elements are drive labels "C:"
int32_t fmf_index_start_scope(FmfEngineHandle h, const char* const* roots, uint32_t n, const char* const* excludes, uint32_t m); // non-elevated folder-walk (ADR-0024); excludes prune matching subtrees at walk time (ADR-0025)
int32_t fmf_index_status(FmfEngineHandle h, FmfVolumeStatus* buf, uint32_t cap, uint32_t* count);
// FmfVolumeStatus.state: Scanning / Ready / Rescanning / Failed
// queries always succeed over "Ready volumes only" (UI judges the partial-result InfoBar by state)

// ── query (synchronous, fast; sort finalized at query time) ──
// options: { sort: Name|Size|Mtime, dir: Asc|Desc, case_mode: Smart|Insensitive|Sensitive,
//            include_hidden_system: bool (default false = exclude H/S attributes and their descendants),
//            regex_mode: u32 (bit0=interpret the whole query as one regex, bit1=scope 0=name/1=full path) }
int32_t fmf_query(FmfEngineHandle h, const char* query_utf8,
                  const FmfQueryOptions* options, FmfResultHandle* out, uint64_t* out_count,
                  FmfBlob** out_trace /* nullable: QueryTrace JSON */);

// ── observability (JSON blob; same "engine allocates+free" pattern as FmfPage) ──
// FmfBlob { data: *const u8, len: u32 } — UTF-8 JSON
int32_t fmf_engine_stats(FmfEngineHandle h, FmfBlob** out); // MetricsSnapshot (recent trace, histograms, USN feed, per-column memory)
int32_t fmf_blob_free(FmfBlob*);
// ── page fetch: an engine-allocated contiguous block (row-header array+string blob). 1 P/Invoke, 1 copy ──
// FmfRow (48 bytes, no padding; fmf-ffi's contract_tests fix size/offset):
//   { entry_ref u64, frn u64, size u64, mtime i64,
//     name_off u32, parent_path_off u32, flags u32, name_len u16, parent_path_len u16 } + trailing blob
// returns FMF_E_STALE = structural_generation mismatch. UI re-issues the same query
int32_t fmf_result_page(FmfResultHandle r, uint64_t offset, uint32_t count, FmfPage** out);
int32_t fmf_page_free(FmfPage* p);
int32_t fmf_result_free(FmfResultHandle r);

// ── diagnostics ──
// len is in/out: in=buffer capacity, out=length written (excluding NUL). Insufficient capacity is silently
// truncated (always NUL-terminated). buf=NULL queries the required size.
int32_t fmf_last_error(char* buf, uint32_t* len);
```

Error code table (shared with the pipe protocol. **Append-only, no renumbering** — contract_tests pin the values): `FMF_OK=0, FMF_E_INVALID_ARG=1, FMF_E_STALE=2, FMF_E_NOT_ADMIN=3, FMF_E_VOLUME=4, FMF_E_QUERY_SYNTAX=5, FMF_E_IO=6, FMF_E_LOCKED=7, FMF_E_PANIC=99`.
`FMF_E_LOCKED` = another process holds the index_dir writer lock (cross-process enforcement of the single-writer invariant; see the "Pipe Protocol" section).

```c
// ── explicit save (materialized in v2) ──
// Snapshot-saves only Ready volumes that are dirty (content_generation advanced since the last save).
// The service calls this internally on a schedule+at stop. Not exposed on the pipe
// (opcode 11 is a number reservation only — client-driven flush spamming is a DoS path that stops USN apply).
int32_t fmf_flush(FmfEngineHandle h);
```

**Intentionally not included**: `fmf_entry_full_path` (unnecessary since a row carries name+parent_path) / query cancel (queries are expected to take tens of ms; the UI drops stale results via the generation counter; only the room to add `fmf_query_cancel` if it ever gets heavy is left).

## Pipe Protocol (v2 service split)

The wire spec between `fmf-service` (privileged service) and the non-privileged UI. This section is canonical. The machine-readable
definitions (error codes, opcodes, event kinds, POD, limits, version numbers) are held as the single canonical source by the
zero-dependency leaf crate **`fmf-contract`**, and `fmf-proto` (the encode/decode implementation),
`fmf-ffi`, and `fmf-service` radiate from it ([ADR-0018](adr/0018-contract-single-source.md);
the former claim "a cdylib cannot be depended on, so constants must be duplicated" was a factual error about Cargo — the only
impossible direction is depending **on** a cdylib). fmf-ffi's contract_tests remain as literal absolute-value pins,
serving as an independent tripwire that detects miss-edits of the canonical source itself.

### Transport

- pipe name: `\\.\pipe\fmf-engine-v2` (the protocol version is in the name; an incompatible change bumps the whole name — v1→v2 was the incompatible change of adding `regex_mode` to `FmfQueryOptions`, growing it 16→20B. ADR-0023)
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

- malformed frame (unknown opcode, len overflow, truncation) = disconnect+`pipe_malformed_frames` counter+warn
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
run with `FMF_BLESS=1` (the ritual for an intentional contract change — [ADR-0018](adr/0018-contract-single-source.md)).

| op | name | FFI mapping | payload (req → resp) |
|---|---|---|---|
| 1 | Hello | `fmf_abi_version` | `{protocol_version:u32}` → `{protocol_version:u32, abi_version:u32, server_pid:u32}` (version mismatch is INVALID_ARG+disconnect) |
| 2 | Subscribe | `fmf_set_event_callback(cb≠NULL)` | empty → empty. events pushed to this connection thereafter |
| 3 | Unsubscribe | `fmf_set_event_callback(NULL)` | empty → empty |
| 4 | ListVolumes | `fmf_list_volumes` | empty → JSON `[{"volume":"C:","state":0,"entries":0}]` (state equals FmfVolumeStatus.state) |
| 5 | IndexStart | `fmf_index_start` | JSON `{"volumes":["C:"]}` → empty (persisted to service.json) |
| 6 | IndexStatus | `fmf_index_status` | empty → JSON (same shape as ListVolumes) |
| 7 | Query | `fmf_query` | `FmfQueryOptions` (20B POD below)+UTF-8 query string (length derived from frame len, no NUL terminator) → `{result_id:u64, count:u64}`+QueryTrace JSON |
| 8 | ResultPage | `fmf_result_page` | `{result_id:u64, offset:u64, count:u32}` → `{row_count:u32, blob_len:u32}` → `FmfRow` (48B)× row_count (densely packed) → string blob (blob_len bytes, WTF-8). `name_off`/`parent_path_off` are byte offsets **relative to the start of the blob** (same layout as the FFI FmfPage) |
| 9 | ResultFree | `fmf_result_free` | `{result_id:u64}` → empty |
| 10 | Stats | `fmf_engine_stats` | empty → MetricsSnapshot JSON (same shape as FFI, snake_case) |
| 11 | (Flush reserved) | `fmf_flush` | **number reserved only, not implemented** — client-driven flush spamming is a local DoS path that stops USN apply by repeatedly holding index.read(). Saving is the service's internal responsibility |
| 12 | ServiceInfo | (service-specific) | empty → JSON `{uptime_ms, connections, version}` |

`FmfQueryOptions` (20B, no padding, LE — pinned by a contract test like FmfRow):
`{ sort:u32@0(0=Name 1=Size 2=Mtime), desc:u32@4(0=Asc 1=Desc),
case_mode:u32@8(0=Smart 1=Insensitive 2=Sensitive), include_hidden_system:u32@12(0/1),
regex_mode:u32@16(bit0=treat the whole query as one regex, bit1=scope 0=name/1=full path, high bits reserved 0) }`

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
- limit 64/connection. On excess, **evict the least-recently-accessed (LRU)**, and a subsequent
  ResultPage for that result_id returns `FMF_E_STALE` (detail includes "evicted" to make it distinguishable from a structural generation change).
  the client recovers via the existing STALE→re-query path

### Single-writer exclusion (cross-process)

- `Engine::new` opens `{index_dir}\.writer.lock` in share-mode 0 and holds it for its lifetime. Failure is
  `FMF_E_LOCKED`. It auto-releases when the OS handle vanishes, so a stale lock never occurs
- the service as the loser (in-proc UI got there first): backoff retry (5s→60s cap)+logs the holding process pid.
  Stops with an exit code that does not trigger an SCM failure-recovery (restart) loop
- the UI as the loser (`--engine=inproc` while the service is running): an explanatory InfoBar ("Service is running.
  To use in-proc, run `just service-stop`")

### Per-machine settings `%ProgramData%\find-my-files\service.json` (service-owned)

```json
{ "volumes": ["C:"], "log_level": "info", "flush_interval_secs": 300, "authorized_sids": ["S-1-5-21-…"] }
```

- `fmf-service install` creates it together with capturing the user SID. IndexStart receipt persists volumes.
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
- **Engine selection** (`EngineClientFactory`): CLI `--fake-engine` / `--engine=pipe|inproc` > settings.json `"engine"` (default `auto`) > auto = pipe 250ms probe → success uses Pipe / on failure **branch on service state (`ServiceSetup.QueryState`)**: if running (=holds writer.lock; a probe failure means the token is unauthorized) then **do not create in-proc even when elevated** (reliably avoids an FMF_E_LOCKED collision), an empty engine with a "re-register" affordance / if absent/stopped and the process is elevated, Ffi (writer.lock is free) / if neither is possible, an explanatory InfoBar+**empty engine** (zero-result `FakeEngineClient.CreateEmpty()`, badge "not connected" — no demo data: fake data has no practical use in a search app)+a "Restart as administrator" button (explicit action only; no automatic runas loop; forwards the non-elevated user's SID via `--setup-owner`). When started elevated in-proc with the service unregistered/stopped, you can set it up with one click in an in-app notification (`ServiceSetup` → `fmf-service install --owner-sid`+`restart`; install is idempotent on the service side, restart reflects the new `authorized_sids`) — an onboarding path that never opens a terminal: normal start → "Restart as administrator" → "Register and start the service" → normal start forever after. The Fake with data is `--fake-engine` (development/UI test) only.
- **Disconnect and reconnect** (`PipeEngineClient`): disconnect = fail in-flight requests immediately with `EngineUnavailableException`, epoch-invalidate surviving `ISearchResult`s (afterwards `GetRangeAsync` → `StaleResultException` = the existing re-query mechanism is the recovery path), reconnect indefinitely with backoff (250ms→5s). The reconnect sequence is canonical in the "Pipe Protocol" section (`VolumeUpdated` events are synthesized and fired from the IndexStatus response). Requests have a default timeout of 10s.
- `SearchResultHandle : SafeHandle`. Page fetches bracket `DangerousAddRef/Release`, and do not release the underlying object even after `Dispose()` until in-flight fetches complete.
- page received→copy to `ResultRow`→**immediately `fmf_page_free`**.
- the callback delegate is held in a client field (prevents GC reclamation). After receipt, to the UI via `DispatcherQueue.TryEnqueue`.
- **Search pipeline responsibility split** (MainViewModel is the composition root only):
  - `SearchOrchestrator` — when and what to search: 50ms debounce (clear is immediate), Dispose of stale results via the generation counter, `RequeryOrigin` classification, bounded Stale retry (1×), exception classification. **An empty query is not sent to the engine** (the product rule that an empty field has no results to return; a match-all enumeration would have its IDs shift every USN tick, so the start screen would redraw forever) — empty screen via `PresentEmpty` (idempotent). **During IME composition the query is held** (`TextCompositionStarted/Ended`; only the committed string flows through the normal debounce). **Focused mode** (focused search) = a pure query rewrite just before passing to the engine (`FocusedQueryRewriter`: add a `!path:` exclusion and one `ext:` whitelist item to each OR group; do not add ext to an explicit `ext:`/`regex:` group, nor an exclusion to a group containing `path:`/`\`) — does not touch the engine; settings in settings.json, ADR-0019.
  - `ResultsPresenter` — presenting results: prefetch the visible-range page **before** publishing, then publish atomically via `VirtualResultList.Reassign` (the old results stay on screen until the new ones are ready=zero blank frames). Count text and viewport placement events.
- two re-query families (`RequeryOrigin` classifies): **type/clear/sort/filter-originated=reset to top** / **IndexChanged/VolumeReady/Stale-originated=save the top visible index→restore, and selection restored best-effort only when an EntryRef in the seed matches**.
- `VirtualResultList` (non-generic IList+INCC+IItemsRangeInfo): **a single instance with the same lifetime as the page** (ItemsSource is x:Bind OneTime — replacing it discards the ListView virtualization state and causes flicker). New results are `Reassign(result, seeds)` = epoch++ → discard the page cache → apply seeds → **emit INCC Reset once** (UI thread only). **A re-query of the same result** (guaranteed by the engine via `QueryTrace.unchanged`: same text+options and the ID sequence memcmp-matches on every volume) is `RefreshInPlace` = epoch++ → swap the handle → in-place fill the visible seed into existing row instances (the MVVM setter notifies only on value change) → **no Reset, count text unchanged** — the screen does not redraw on the re-query that idle USN traffic (logs, telemetry, etc.) triggers every 200ms. In-place updated size/mtime update only the cells whose value changed. The indexer never fetches and returns a placeholder (**out of range throws immediately** — no negative index, no fabricated fake page). On `RangesChanged`, background-fetch the visible range ±1 page in 64-row units→fill properties of existing ResultRows. Completion of an old-epoch fetch is silently discarded. Page LRU limit 4096 rows. Hard STALE receipt→`BecameStale` (only on epoch match)→ the Orchestrator re-queries.
- **IList contract invariant (do not falsely affirm membership)**: XAML blindly trusts the answers of `Contains`/`IndexOf`/`GetAt` via the WinRT adapter. A false "absent" is fixed by container re-realization, but a false "present" causes a crash deep in XAML at `GetAt(staleIndex)` (proven: the root of the `Int32.MaxValue-1` exception that reliably reproduced on search-with-results→clear-all). Membership is defined as "index is below Count AND the corresponding slot in the current page cache is that same instance". A row of an old result, a row of an LRU-evicted page, and a temporary row for enumeration always answer absent. Enumeration/CopyTo do not disturb the virtualization state (LRU). The UI-thread check of the mutation family (Reassign/RefreshInPlace) is always active in Release.

## Error Handling and Diagnostics (principle: "never crash, never hang, never go silent")

Every anomaly always reaches 3 paths: **(1) the log file (2) the diag ring (=auto-displayed in the F12 panel/fmf stats) (3) the UI InfoBar**. No telemetry is sent (local only).

- **Logs**: engine=`%ProgramData%\find-my-files\logs\engine.log` (daily rotation, filter via the `FMF_LOG` env var), app=`%APPDATA%\find-my-files\logs\app.log` (one-generation rotation at 2MB)
- **diag ring** (fmf-core::diag): holds the most recent 128 tracing events at WARN or above+panics (with backtrace). Always included in `MetricsSnapshot.recent_errors`
- **panic**: caught by a global hook→log+ring. The volume thread has a `catch_unwind` firewall, so even on panic the UI always receives `VolumeFailed` (no silent hang)
- **Event kind 6 `FMF_EVENT_ENGINE_ERROR`**: a POD notification that a diag event occurred (entries=severity 1=warn/2=error/3=panic). Detail text is pulled from the stats JSON (push notification+pull detail)
- **Degradation recording convention (ADR-0018)**: a degradation path uses `fmf_core::degrade!` (the only way to do tracing::warn!+counter increment **atomically**; `rg degrade!` = enumerates all degradation paths). The batch path inside scan is the sole exception, returning the degradation in a `ScanStats` field and mapping it to counters+warn in one place at the worker layer (do not scatter the macro across the hot path). The boundary crates (fmf-ffi / fmf-service) forbid `unwrap_or_default` via disallowed-methods in clippy.toml — a silent fallback is rejected at compile time
- **Canonical source of counter names**: `fmf-contract::counters::COUNTER_NAMES` (C#'s CountersData is generated by gen-contract, and fmf-core's golden test reconciles CountersSnapshot's serde keys with the roster — a missing addition is mechanically detected)
- **Degradation counters** (`MetricsSnapshot.counters`, shown in F12 if nonzero): stat_fetch_failures / usn_batches_truncated / snapshot_load_failures / snapshot_save_failures / deferred_names_unresolved / corrupt_mft_records / journal_rescans / scan_pipeline_fallbacks (scan read-ahead I/O thread startup failure→degrade to sequential read) / offset_table_rebuild_fallbacks (offset table watermark mismatch→degrade to full rebuild) / lazy_perm_rebuild_fallbacks (the same kind of defense for the lazy sort permutation) / compaction_aborts (generation mismatch during compaction→discard the copy. Detects a break of the single-writer invariant) / pipe_malformed_frames (malformed frame→disconnect) / pipe_events_dropped (event bounded-queue overflow→drop oldest) / pipe_connections_rejected (instance limit exceeded) / deferred_name_cache_overflow (extension-record name cache full→degrade to disk read) / deferred_name_read_failures (disk-read failure of lazy name resolution) / pipe_results_evicted (LRU eviction of a result handle) / trace_serialize_failures (QueryTrace JSON-ification failure→respond with an empty trace)
- **Single implementation of error detail**: `fmf_core::diag::error_chain` (joins all causes, **4KiB limit+"…" truncation**) — both FFI `fmf_last_error` and the pipe error-response payload use this
- **Single home of diagnostics init**: `fmf_core::diag::init_diag(log_dir, level)` (logging+panic hook+diag ring connect, idempotent) is called by all entry points: FFI / service / CLI. log_dir resolution is `resolve_log_dir`: **explicit specification (config/CLI) > a `logs` subdir of the engine's `index_dir`** (co-located with the index, so it shares the index's writable, non-machine-wide pollution domain) — there is no machine-wide fallback (`%ProgramData%` dirtied the machine for non-elevated callers and panicked when unwritable); the machine service still logs to `%ProgramData%\find-my-files\logs` by passing it explicitly. This priority is implemented in only this one place
- **C# convention**: fire-and-forget always uses `task.Forget(area)` (exception→app.log+InfoBar). Shell operations go through `ShellOps`. A global exception handler writes a crash marker and notifies on the next start
- **Diagnostics copy**: the F12 panel's "Copy diagnostics" = stats JSON+tail of app.log+environment info

| FFI code | meaning | UI behavior | retry |
|---|---|---|---|
| FMF_E_QUERY_SYNTAX(5) | query syntax error | shown in the status bar | fix input |
| FMF_E_STALE(2) | structural generation change | auto re-issue the same query | automatic |
| FMF_E_NOT_ADMIN(3) | insufficient elevation | InfoBar+explanation | restart |
| FMF_E_LOCKED(7) | index_dir held by another engine | InfoBar+explanation ("Service is running. Use in-proc after just service-stop") | restart after stopping the service |
| FMF_E_PANIC(99) | panic inside the engine | InfoBar+pointer to engine.log | not possible (report) |
| others (1,4,6) | argument/volume/IO | InfoBar | depends |

## Latency Budget (breakdown of the change→on-screen ≤1s AC)

USN batch commit ≤100ms + engine IndexChanged debounce 200ms (the only throttle) + UI re-query ≤100ms + render ≤100ms = **≤500ms** (2× margin). Do not place an additional throttle on the UI side.

Additional budget for the pipe path (**canonical here** — other docs' numbers reference this section): ResultPage 64-row round trip p99
**≤5ms** (provisional — the loopback integration test asserts it, to be finalized by measurement). Continuously observed via F12's `PageRttEwma`.
Event push is one hop after the debounce above, so the budget structure does not change.

pipe test gates: protocol round-trip and loopback integration (unique pipe name+
`insert_ready_volume`) run unconditionally under non-elevated `cargo test`. The C# client × real fmf-service
integration is `FMF_PIPE_TESTS=1` (`just test-pipe`). The service E2E using real volumes is, as before,
`FMF_ADMIN_TESTS=1` (elevated).
