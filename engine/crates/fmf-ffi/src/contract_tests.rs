//! FFI contract tests — docs/ARCHITECTURE.md is the canonical contract.
//!
//! Three families:
//! 1. **ABI layout pins**: struct sizes/offsets that the C# marshaling layer
//!    (`LayoutKind.Explicit` mirrors) depends on. `FmfRow` = 56 bytes is
//!    contractual; for the other structs the contract does not spell out a
//!    byte layout, so the *current* layout is pinned here as a regression
//!    detector — any drift must be a conscious, doc-updating change.
//! 2. **Null/invalid-argument matrix**: every export's `FMF_E_INVALID_ARG`
//!    paths, plus the "null is OK" contract of the free functions.
//! 3. **Behavior roundtrips**: strict `fmf_last_error` probe/copy, the query
//!    syntax-error cause chain, and page/blob packing.
//!
//! Everything here runs unelevated: `fmf_index_start` is never pointed at a
//! real volume; ready volumes are injected via `Engine::insert_ready_volume`.

use std::ffi::{CString, c_char, c_void};
use std::mem::offset_of;
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

use fmf_core::engine::Engine;
use fmf_core::index::testutil::TestDir;

use crate::blob::{FmfBlob, fmf_blob_free, fmf_engine_stats};
use crate::error::fmf_last_error;
use crate::events::{
    CallbackSink, FMF_EVENT_ENGINE_ERROR, FMF_EVENT_INDEX_CHANGED, FMF_EVENT_PROGRESS,
    FMF_EVENT_RESCAN_STARTED, FMF_EVENT_VOLUME_FAILED, FMF_EVENT_VOLUME_READY, FmfEvent,
    FmfEventCb, fmf_set_event_callback,
};
use crate::handle::{engine, fmf_abi_version, fmf_engine_create, fmf_engine_destroy, fmf_flush};
use crate::query_control::{
    fmf_query_control_cancel, fmf_query_control_create, fmf_query_control_free,
};
use crate::results::{
    FmfPage, FmfQueryOptions, FmfRow, fmf_page_free, fmf_query as raw_fmf_query, fmf_result_free,
    fmf_result_page, result,
};
use crate::volumes::{FmfVolumeStatus, fmf_index_start, fmf_index_status, fmf_list_volumes};

// Real, second-aligned FILETIMEs that round-trip through the u32-seconds
// mtime column (ADR-0031); pre-1970 small ints collapse to the 0 sentinel.
const MT_ALPHA: i64 = 132_854_688_000_000_000; // ≈ 2022-01-01
const MT_BETA: i64 = 133_170_048_000_000_000; // ≈ 2023-01-01
use crate::{
    FMF_E_CANCELLED, FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_LOCKED, FMF_E_NOT_ADMIN, FMF_E_PANIC,
    FMF_E_QUERY_SYNTAX, FMF_E_STALE, FMF_E_VOLUME, FMF_OK,
};

// ── helpers ─────────────────────────────────────────────────────────────

/// Creates an engine against a fresh [`TestDir`] — no volume is touched, so
/// this needs no elevation (`Engine::new` only builds in-memory state). The
/// guard is returned because it must outlive the engine handle: callers
/// bind it (`let (h, _dir) = …`) and call `destroy` before scope end.
fn create_engine() -> (*mut c_void, TestDir) {
    let dir = TestDir::new();
    let cfg = serde_json::json!({
        "index_dir": dir.join("index").to_string_lossy(),
        "log_dir": dir.join("logs").to_string_lossy(),
        "log_level": "warn",
    })
    .to_string();
    let cfg = CString::new(cfg).unwrap();
    let mut h: *mut c_void = ptr::null_mut();
    let rc = unsafe { fmf_engine_create(cfg.as_ptr(), &raw mut h) };
    assert_eq!(
        rc,
        FMF_OK,
        "fmf_engine_create failed: {}",
        read_last_error()
    );
    assert!(!h.is_null());
    (h, dir)
}

fn destroy(h: *mut c_void) {
    assert_eq!(unsafe { fmf_engine_destroy(h) }, FMF_OK);
}

/// Reads the thread-local detail message with an ample buffer.
fn read_last_error() -> String {
    let mut buf = [0u8; 1024];
    let mut len: u32 = buf.len() as u32;
    assert_eq!(
        unsafe { fmf_last_error(buf.as_mut_ptr(), &raw mut len) },
        FMF_OK
    );
    String::from_utf8(buf[..len as usize].to_vec()).expect("last_error is UTF-8")
}

fn default_opts() -> FmfQueryOptions {
    FmfQueryOptions {
        sort: 0,      // Name
        desc: 0,      // Asc
        case_mode: 0, // Smart
        include_hidden_system: 0,
        regex_mode: 0, // whole-query regex off
        _reserved: 0,
        presentation_basis: 0,
    }
}

#[test]
fn query_control_pre_cancel_is_not_lost_and_ids_fail_closed() {
    let (h, _dir) = create_engine();
    let mut control_id = 0;
    assert_eq!(
        unsafe { fmf_query_control_create(h, &raw mut control_id) },
        FMF_OK
    );
    assert_ne!(control_id, 0);
    assert_eq!(fmf_query_control_cancel(control_id), FMF_OK);
    assert_eq!(
        fmf_query_control_cancel(control_id),
        FMF_OK,
        "cancellation is idempotent while the lifecycle is live"
    );

    let query = CString::new("").unwrap();
    let options = default_opts();
    let mut result_handle = ptr::without_provenance_mut::<c_void>(999);
    let mut count = 999;
    let mut trace = ptr::without_provenance_mut::<FmfBlob>(999);
    assert_eq!(
        unsafe {
            raw_fmf_query(
                h,
                query.as_ptr(),
                &raw const options,
                control_id,
                &raw mut result_handle,
                &raw mut count,
                &raw mut trace,
            )
        },
        FMF_E_CANCELLED
    );
    assert!(result_handle.is_null());
    assert_eq!(count, 0);
    assert!(trace.is_null());

    let query_error = read_last_error();
    assert!(query_error.contains("cancelled"));
    assert_eq!(fmf_query_control_free(control_id), FMF_OK);
    assert_eq!(
        read_last_error(),
        query_error,
        "mandatory query-control cleanup must preserve the query diagnostic"
    );
    assert_eq!(fmf_query_control_free(control_id), FMF_E_INVALID_ARG);
    assert_eq!(fmf_query_control_cancel(control_id), FMF_E_INVALID_ARG);
    assert_eq!(fmf_query_control_cancel(u64::MAX), FMF_E_INVALID_ARG);

    let mut replacement = 0;
    assert_eq!(
        unsafe { fmf_query_control_create(h, &raw mut replacement) },
        FMF_OK
    );
    assert_ne!(replacement, control_id, "control IDs are never reused");
    assert_eq!(
        fmf_query_control_cancel(control_id),
        FMF_E_INVALID_ARG,
        "a stale ID cannot cancel its replacement"
    );
    assert_eq!(fmf_query_control_free(replacement), FMF_OK);
    destroy(h);
}

/// Compatibility helper for the bulk of the ABI behavior suite: each call
/// owns the new v5 query-control lifecycle. Dedicated tests below exercise
/// pre-cancel/forgery/double-free races directly against `raw_fmf_query`.
unsafe fn fmf_query(
    h: *mut c_void,
    query_utf8: *const c_char,
    options: *const FmfQueryOptions,
    out_handle: *mut *mut c_void,
    out_count: *mut u64,
    out_trace: *mut *mut FmfBlob,
) -> i32 {
    let mut control_id = 0;
    let create = unsafe { fmf_query_control_create(h, &raw mut control_id) };
    if create != FMF_OK {
        return create;
    }
    let result = unsafe {
        raw_fmf_query(
            h, query_utf8, options, control_id, out_handle, out_count, out_trace,
        )
    };
    assert_eq!(fmf_query_control_free(control_id), FMF_OK);
    result
}

fn json_from_blob(blob: *mut FmfBlob) -> serde_json::Value {
    assert!(!blob.is_null());
    let b = unsafe { blob.as_ref() }.expect("blob is non-null");
    let bytes = unsafe { std::slice::from_raw_parts(b.data, b.len as usize) };
    serde_json::from_slice(bytes).expect("blob is UTF-8 JSON")
}

fn blob_owner_id(blob: *mut FmfBlob) -> u64 {
    let owner_id = unsafe { blob.as_ref() }.expect("blob is non-null").owner_id;
    assert_ne!(owner_id, 0, "successful blob allocations have an owner ID");
    owner_id
}

fn page_owner_id(page: *mut FmfPage) -> u64 {
    let owner_id = unsafe { page.as_ref() }.expect("page is non-null").owner_id;
    assert_ne!(owner_id, 0, "successful page allocations have an owner ID");
    owner_id
}

/// Engine with one injected Ready volume ("C:", two files) — the unelevated
/// stand-in for a real MFT scan.
fn ready_engine() -> (*mut c_void, TestDir) {
    use fmf_core::index::{Frn, RawEntry, VolumeIndexBuilder};

    let (h, dir) = create_engine();
    let mut b = VolumeIndexBuilder::new("C:", 5);
    let alpha: Vec<u16> = "alpha.txt".encode_utf16().collect();
    b.push(RawEntry {
        parent_frn: Frn(5),
        frn: Frn((1 << 48) | 0x64),
        name_utf16: &alpha,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 1234,
        mtime: MT_ALPHA,
    });
    let beta: Vec<u16> = "beta.log".encode_utf16().collect();
    b.push(RawEntry {
        parent_frn: Frn(5),
        frn: Frn((1 << 48) | 0x65),
        name_utf16: &beta,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 99,
        mtime: MT_BETA,
    });
    // Registry lookup clones the Arc exactly like a real FFI entry.
    let handle = engine(h).expect("engine handle is registered");
    let _active = handle.enter().expect("engine handle is active");
    handle.engine.insert_ready_volume("C:", b.finish());
    (h, dir)
}

unsafe extern "C" fn noop_event_cb(_ev: *const FmfEvent, _user: *mut c_void) {}

struct BlockingCallback {
    entered: Barrier,
    release: (Mutex<bool>, Condvar),
    calls: AtomicUsize,
}

unsafe extern "C" fn blocking_event_cb(_ev: *const FmfEvent, user: *mut c_void) {
    let state = unsafe { &*user.cast::<BlockingCallback>() };
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.entered.wait();
    let (lock, wake) = &state.release;
    let mut released = lock.lock().expect("release lock");
    while !*released {
        released = wake.wait(released).expect("release wait");
    }
}

struct ReentrantCallback {
    engine_id: usize,
    code: AtomicI32,
}

unsafe extern "C" fn unregister_inside_callback(_ev: *const FmfEvent, user: *mut c_void) {
    let state = unsafe { &*user.cast::<ReentrantCallback>() };
    let handle = std::ptr::without_provenance_mut(state.engine_id);
    let code = unsafe { fmf_set_event_callback(handle, None, ptr::null_mut()) };
    state.code.store(code, Ordering::SeqCst);
}

// ── 1. ABI layout pins ──────────────────────────────────────────────────

#[test]
fn error_codes_match_contract_table() {
    // ARCHITECTURE.md: FMF_OK=0, INVALID_ARG=1, STALE=2, NOT_ADMIN=3,
    // VOLUME=4, QUERY_SYNTAX=5, IO=6, LOCKED=7, PANIC=99 (shared with the
    // pipe protocol — renumbering is a breaking protocol change).
    assert_eq!(FMF_OK, 0);
    assert_eq!(FMF_E_INVALID_ARG, 1);
    assert_eq!(FMF_E_STALE, 2);
    assert_eq!(FMF_E_NOT_ADMIN, 3);
    assert_eq!(FMF_E_VOLUME, 4);
    assert_eq!(FMF_E_QUERY_SYNTAX, 5);
    assert_eq!(FMF_E_IO, 6);
    assert_eq!(FMF_E_LOCKED, 7);
    assert_eq!(FMF_E_CANCELLED, 8);
    assert_eq!(FMF_E_PANIC, 99);
}

#[test]
fn abi_version_is_pinned() {
    assert_eq!(fmf_abi_version(), 5);
}

#[test]
fn contract_values_are_pinned_literally() {
    // The single source (fmf-contract) removed the duplicate definitions;
    // these literal pins are the independent tripwire that catches an
    // accidental edit of the canonical file itself (ADR-0018). Append-only
    // table: renumbering is a breaking protocol change.
    assert_eq!(fmf_contract::versions::PROTOCOL_VERSION, 4);
    assert_eq!(fmf_contract::versions::ABI_VERSION, 5);
    assert_eq!(fmf_contract::versions::PIPE_NAME, r"\\.\pipe\fmf-engine-v4");
    assert_eq!(fmf_contract::opcodes::HELLO, 1);
    assert_eq!(fmf_contract::opcodes::SUBSCRIBE, 2);
    assert_eq!(fmf_contract::opcodes::UNSUBSCRIBE, 3);
    assert_eq!(fmf_contract::opcodes::LIST_VOLUMES, 4);
    assert_eq!(fmf_contract::opcodes::INDEX_START, 5);
    assert_eq!(fmf_contract::opcodes::INDEX_STATUS, 6);
    assert_eq!(fmf_contract::opcodes::QUERY, 7);
    assert_eq!(fmf_contract::opcodes::RESULT_PAGE, 8);
    assert_eq!(fmf_contract::opcodes::RESULT_FREE, 9);
    assert_eq!(fmf_contract::opcodes::STATS, 10);
    assert_eq!(fmf_contract::opcodes::SERVICE_INFO, 12);
    assert_eq!(fmf_contract::opcodes::QUERY_CANCEL, 13);
    assert_eq!(fmf_contract::limits::MAX_PAYLOAD_LEN, 16 * 1024 * 1024);
    assert_eq!(fmf_contract::limits::MAX_RESULTS_PER_CONN, 64);
    assert_eq!(fmf_contract::limits::EVENT_QUEUE_CAP, 256);
    assert_eq!(fmf_contract::options::SortKey::Name as u32, 0);
    assert_eq!(fmf_contract::options::SortKey::Size as u32, 1);
    assert_eq!(fmf_contract::options::SortKey::Mtime as u32, 2);
    assert_eq!(fmf_contract::options::CaseMode::Smart as u32, 0);
    assert_eq!(fmf_contract::options::CaseMode::Insensitive as u32, 1);
    assert_eq!(fmf_contract::options::CaseMode::Sensitive as u32, 2);
    assert_eq!(fmf_contract::options::VolumeState::Scanning as u32, 0);
    assert_eq!(fmf_contract::options::VolumeState::Ready as u32, 1);
    assert_eq!(fmf_contract::options::VolumeState::Rescanning as u32, 2);
    assert_eq!(fmf_contract::options::VolumeState::Failed as u32, 3);
}

#[test]
fn fmf_row_layout_is_56_bytes_no_implicit_padding() {
    // Contractual: "56-byte row, no implicit padding", mirrored by C#
    // LayoutKind.Explicit. The reserved tail word is part of v3 and must
    // remain zero on the wire.
    assert_eq!(size_of::<FmfRow>(), 56);
    assert_eq!(align_of::<FmfRow>(), 8);
    assert_eq!(offset_of!(FmfRow, entry_ref), 0);
    assert_eq!(offset_of!(FmfRow, frn), 8);
    assert_eq!(offset_of!(FmfRow, size), 16);
    assert_eq!(offset_of!(FmfRow, mtime), 24);
    assert_eq!(offset_of!(FmfRow, name_off), 32);
    assert_eq!(offset_of!(FmfRow, parent_path_off), 36);
    assert_eq!(offset_of!(FmfRow, flags), 40);
    assert_eq!(offset_of!(FmfRow, name_len), 44);
    assert_eq!(offset_of!(FmfRow, parent_path_len), 48);
    assert_eq!(offset_of!(FmfRow, _reserved), 52);
}

#[test]
fn fmf_event_layout_pinned() {
    // Not byte-specified in the contract ("POD" only) — current layout
    // pinned as a regression detector for the C# mirror.
    assert_eq!(size_of::<FmfEvent>(), 32);
    assert_eq!(align_of::<FmfEvent>(), 8);
    assert_eq!(offset_of!(FmfEvent, kind), 0);
    assert_eq!(offset_of!(FmfEvent, _pad), 4);
    assert_eq!(offset_of!(FmfEvent, entries), 8);
    assert_eq!(offset_of!(FmfEvent, volume), 16);

    // Event kinds: 6 (ENGINE_ERROR) is named in the contract; the rest are
    // pinned at their current values.
    assert_eq!(FMF_EVENT_PROGRESS, 1);
    assert_eq!(FMF_EVENT_VOLUME_READY, 2);
    assert_eq!(FMF_EVENT_INDEX_CHANGED, 3);
    assert_eq!(FMF_EVENT_RESCAN_STARTED, 4);
    assert_eq!(FMF_EVENT_VOLUME_FAILED, 5);
    assert_eq!(FMF_EVENT_ENGINE_ERROR, 6);

    // Option<fn> niche: the callback marshals as a plain (nullable) C
    // function pointer — required for "cb=NULL unregisters".
    assert_eq!(size_of::<FmfEventCb>(), size_of::<usize>());
}

#[test]
fn fmf_volume_status_layout_pinned() {
    // Not byte-specified in the contract — current layout pinned.
    assert_eq!(size_of::<FmfVolumeStatus>(), 32);
    assert_eq!(align_of::<FmfVolumeStatus>(), 8);
    assert_eq!(offset_of!(FmfVolumeStatus, label), 0);
    assert_eq!(offset_of!(FmfVolumeStatus, state), 16);
    assert_eq!(offset_of!(FmfVolumeStatus, _pad), 20);
    assert_eq!(offset_of!(FmfVolumeStatus, entries), 24);
}

#[test]
fn fmf_page_layout_pinned() {
    // Not byte-specified in the contract — current layout pinned
    // (pointers are 8 bytes: this project is 64-bit Windows only).
    assert_eq!(size_of::<FmfPage>(), 40);
    assert_eq!(align_of::<FmfPage>(), 8);
    assert_eq!(offset_of!(FmfPage, row_count), 0);
    assert_eq!(offset_of!(FmfPage, _pad), 4);
    assert_eq!(offset_of!(FmfPage, rows), 8);
    assert_eq!(offset_of!(FmfPage, blob), 16);
    assert_eq!(offset_of!(FmfPage, blob_len), 24);
    assert_eq!(offset_of!(FmfPage, _pad2), 28);
    assert_eq!(offset_of!(FmfPage, owner_id), 32);
}

#[test]
fn fmf_query_options_layout_pinned() {
    // Contract lists the option fields but not a byte layout — pinned.
    assert_eq!(size_of::<FmfQueryOptions>(), 32);
    assert_eq!(align_of::<FmfQueryOptions>(), 8);
    assert_eq!(offset_of!(FmfQueryOptions, sort), 0);
    assert_eq!(offset_of!(FmfQueryOptions, desc), 4);
    assert_eq!(offset_of!(FmfQueryOptions, case_mode), 8);
    assert_eq!(offset_of!(FmfQueryOptions, include_hidden_system), 12);
    assert_eq!(offset_of!(FmfQueryOptions, regex_mode), 16);
    assert_eq!(offset_of!(FmfQueryOptions, _reserved), 20);
    assert_eq!(offset_of!(FmfQueryOptions, presentation_basis), 24);
}

#[test]
fn fmf_blob_layout_pinned() {
    // Contract: { data: *const u8, len: u32, owner_id: u64 };
    // trailing pad pinned.
    assert_eq!(size_of::<FmfBlob>(), 24);
    assert_eq!(align_of::<FmfBlob>(), 8);
    assert_eq!(offset_of!(FmfBlob, data), 0);
    assert_eq!(offset_of!(FmfBlob, len), 8);
    assert_eq!(offset_of!(FmfBlob, _pad), 12);
    assert_eq!(offset_of!(FmfBlob, owner_id), 16);
}

// ── 2. Null/invalid-argument matrix ─────────────────────────────────────

#[test]
fn engine_create_rejects_bad_args() {
    let mut out: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(ptr::null(), &raw mut out) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_engine_create(c"{}".as_ptr(), ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    let bad_utf8: [u8; 2] = [0xFF, 0x00];
    assert_eq!(
        unsafe { fmf_engine_create(bad_utf8.as_ptr().cast::<c_char>(), &raw mut out) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_engine_create(c"not json".as_ptr(), &raw mut out) },
        FMF_E_INVALID_ARG
    );
    // index_dir is a required config key.
    assert_eq!(
        unsafe { fmf_engine_create(c"{}".as_ptr(), &raw mut out) },
        FMF_E_INVALID_ARG
    );
    assert!(read_last_error().contains("index_dir"));
    assert!(out.is_null(), "out must stay untouched on failure");

    let unknown = CString::new(
        serde_json::json!({
            "index_dir": "unused",
            "directory_scan_fallback": true,
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        unsafe { fmf_engine_create(unknown.as_ptr(), &raw mut out) },
        FMF_E_INVALID_ARG
    );
    assert!(
        read_last_error().contains("unknown field"),
        "config must fail closed on misspelled/obsolete keys"
    );
}

#[test]
fn zero_is_ok_for_owner_frees_and_null_is_ok_for_result_free_but_not_destroy() {
    assert_eq!(fmf_blob_free(0), FMF_OK);
    assert_eq!(fmf_page_free(0), FMF_OK);
    assert_eq!(unsafe { fmf_result_free(ptr::null_mut()) }, FMF_OK);
    // fmf_engine_destroy is not free-like: a null handle is an error.
    assert_eq!(
        unsafe { fmf_engine_destroy(ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
}

#[test]
fn destroyed_engine_ids_are_never_dereferenced_or_reused() {
    let (h, _dir) = create_engine();
    let admitted = engine(h).expect("registered engine");

    destroy(h);
    assert!(
        admitted.engine.status().is_empty(),
        "in-flight Arc remains safe"
    );
    assert!(engine(h).is_err(), "removed id rejects new admissions");
    assert_eq!(unsafe { fmf_engine_destroy(h) }, FMF_E_INVALID_ARG);
    assert!(read_last_error().contains("already destroyed"));
}

#[test]
fn flush_null_matrix_and_roundtrip() {
    assert_eq!(unsafe { fmf_flush(ptr::null_mut()) }, FMF_E_INVALID_ARG);
    // Roundtrip on an injected Ready volume: flush succeeds and writes the
    // snapshot file the engine layer is contracted to produce.
    let (h, _dir) = ready_engine();
    assert_eq!(unsafe { fmf_flush(h) }, FMF_OK);
    // Second flush is also FMF_OK — "nothing dirty" is success, not an error.
    assert_eq!(unsafe { fmf_flush(h) }, FMF_OK);
    destroy(h);
}

#[test]
fn second_engine_on_same_index_dir_reports_locked() {
    let dir = TestDir::new();
    let cfg = serde_json::json!({
        "index_dir": dir.join("index").to_string_lossy(),
        "log_dir": dir.join("logs").to_string_lossy(),
        "log_level": "warn",
    })
    .to_string();
    let cfg = CString::new(cfg).unwrap();

    let mut first: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(cfg.as_ptr(), &raw mut first) },
        FMF_OK
    );

    let mut second: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(cfg.as_ptr(), &raw mut second) },
        FMF_E_LOCKED
    );
    assert!(second.is_null(), "no handle on a locked dir");
    assert!(
        read_last_error().contains("locked"),
        "detail must explain the lock: {}",
        read_last_error()
    );

    destroy(first);
    // The lock dies with the engine — the dir is usable again.
    let mut third: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(cfg.as_ptr(), &raw mut third) },
        FMF_OK
    );
    destroy(third);
}

#[test]
fn set_event_callback_matrix() {
    assert_eq!(
        unsafe { fmf_set_event_callback(ptr::null_mut(), Some(noop_event_cb), ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    let (h, _dir) = create_engine();
    assert_eq!(
        unsafe { fmf_set_event_callback(h, Some(noop_event_cb), ptr::null_mut()) },
        FMF_OK
    );
    // Contract: cb = NULL unregisters.
    assert_eq!(
        unsafe { fmf_set_event_callback(h, None, ptr::null_mut()) },
        FMF_OK
    );
    destroy(h);
}

#[test]
fn callback_deactivation_waits_for_in_flight_and_blocks_precloned_dispatch() {
    let state = Arc::new(BlockingCallback {
        entered: Barrier::new(2),
        release: (Mutex::new(false), Condvar::new()),
        calls: AtomicUsize::new(0),
    });
    let sink = Arc::new(CallbackSink::new(
        blocking_event_cb,
        Arc::as_ptr(&state).cast_mut().cast(),
    ));
    let precloned = sink.clone();
    let payload = FmfEvent::new(FMF_EVENT_PROGRESS, 1, "C:");

    let dispatch = sink.clone();
    let callback_thread = std::thread::spawn(move || dispatch.invoke(&payload));
    state.entered.wait();

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let stopping = sink;
    let unregister_thread = std::thread::spawn(move || {
        stopping.deactivate_and_wait();
        done_tx.send(()).expect("report quiescence");
    });
    assert!(
        done_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "unregister returned while foreign callback still used its user token"
    );

    let (lock, wake) = &state.release;
    *lock.lock().expect("release lock") = true;
    wake.notify_all();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("unregister completes after callback exits");
    callback_thread.join().expect("callback thread");
    unregister_thread.join().expect("unregister thread");

    precloned.invoke(&FmfEvent::new(FMF_EVENT_PROGRESS, 2, "C:"));
    assert_eq!(
        state.calls.load(Ordering::SeqCst),
        1,
        "a closure cloned before unregister must become inert"
    );
}

#[test]
fn callback_lifecycle_reentry_is_rejected_instead_of_self_deadlocking() {
    let (h, _dir) = create_engine();
    let state = ReentrantCallback {
        engine_id: h.addr(),
        code: AtomicI32::new(i32::MIN),
    };
    let sink = CallbackSink::new(
        unregister_inside_callback,
        std::ptr::from_ref(&state).cast_mut().cast(),
    );
    sink.invoke(&FmfEvent::new(FMF_EVENT_PROGRESS, 1, "C:"));
    assert_eq!(state.code.load(Ordering::SeqCst), FMF_E_INVALID_ARG);
    assert!(read_last_error().contains("cannot be called"));
    destroy(h);
}

#[test]
fn list_volumes_requires_a_live_engine_and_count() {
    let mut count = u32::MAX;
    assert_eq!(
        unsafe { fmf_list_volumes(ptr::null_mut(), ptr::null_mut(), 0, &raw mut count) },
        FMF_E_INVALID_ARG
    );
    let (h, _dir) = create_engine();
    assert_eq!(
        unsafe { fmf_list_volumes(h, ptr::null_mut(), 0, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_list_volumes(h, ptr::null_mut(), 0, &raw mut count) },
        FMF_OK
    );
    assert_ne!(count, u32::MAX, "count must be written");
    destroy(h);
}

#[test]
fn index_start_null_matrix() {
    assert_eq!(
        unsafe { fmf_index_start(ptr::null_mut(), ptr::null(), 0) },
        FMF_E_INVALID_ARG
    );
    let (h, _dir) = create_engine();
    assert_eq!(
        unsafe { fmf_index_start(h, ptr::null(), 3) },
        FMF_E_INVALID_ARG
    );
    // n = 0 with a null array is a valid no-op (nothing to index — still
    // unelevated-safe: no volume thread is spawned).
    assert_eq!(unsafe { fmf_index_start(h, ptr::null(), 0) }, FMF_OK);
    // A null *element* is rejected too.
    let one_null: [*const c_char; 1] = [ptr::null()];
    assert_eq!(
        unsafe { fmf_index_start(h, one_null.as_ptr(), 1) },
        FMF_E_INVALID_ARG
    );

    let available = Engine::list_ntfs_volumes();
    if let Some(label) = available.first() {
        let first = CString::new(label.as_str()).unwrap();
        let duplicate = CString::new(label.to_ascii_lowercase()).unwrap();
        let duplicate_request = [first.as_ptr(), duplicate.as_ptr()];
        assert_eq!(
            unsafe { fmf_index_start(h, duplicate_request.as_ptr(), 2) },
            FMF_E_INVALID_ARG,
            "canonical duplicates must reject before any real volume is started"
        );
    }

    let unavailable = (b'A'..=b'Z')
        .map(|letter| format!("{}:", char::from(letter)))
        .find(|label| !available.contains(label))
        .expect("a test host cannot have all 26 drive letters as fixed NTFS volumes");
    let unavailable = CString::new(unavailable).unwrap();
    let unavailable_request = [unavailable.as_ptr()];
    assert_eq!(
        unsafe { fmf_index_start(h, unavailable_request.as_ptr(), 1) },
        FMF_E_INVALID_ARG
    );
    let mut status_count = u32::MAX;
    assert_eq!(
        unsafe { fmf_index_status(h, ptr::null_mut(), 0, &raw mut status_count) },
        FMF_OK
    );
    assert_eq!(
        status_count, 0,
        "rejected requests must not create a slot or worker"
    );
    destroy(h);
}

#[test]
fn index_status_null_matrix() {
    let mut count = u32::MAX;
    assert_eq!(
        unsafe { fmf_index_status(ptr::null_mut(), ptr::null_mut(), 0, &raw mut count) },
        FMF_E_INVALID_ARG
    );
    let (h, _dir) = create_engine();
    assert_eq!(
        unsafe { fmf_index_status(h, ptr::null_mut(), 0, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    // count alone (buf = NULL) is the size-probe pattern.
    assert_eq!(
        unsafe { fmf_index_status(h, ptr::null_mut(), 0, &raw mut count) },
        FMF_OK
    );
    assert_eq!(count, 0, "no volumes were registered");
    destroy(h);
}

#[test]
fn query_null_matrix() {
    let (h, _dir) = create_engine();
    let q = CString::new("foo").unwrap();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = 0;

    assert_eq!(
        unsafe {
            fmf_query(
                ptr::null_mut(),
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                ptr::null(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                ptr::null(),
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                ptr::null_mut(),
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    // Non-UTF-8 query text is an argument error, not a syntax error.
    let bad_utf8: [u8; 2] = [0xFF, 0x00];
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                bad_utf8.as_ptr().cast::<c_char>(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    assert!(rh.is_null(), "no handle may be allocated on any failure");
    destroy(h);
}

#[test]
fn engine_stats_null_matrix_and_json_roundtrip() {
    let mut blob: *mut FmfBlob = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_stats(ptr::null_mut(), &raw mut blob) },
        FMF_E_INVALID_ARG
    );
    let (h, _dir) = create_engine();
    assert_eq!(
        unsafe { fmf_engine_stats(h, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(unsafe { fmf_engine_stats(h, &raw mut blob) }, FMF_OK);
    // Contract: engine-allocated UTF-8 JSON, released with fmf_blob_free.
    assert!(json_from_blob(blob).is_object());
    assert_eq!(fmf_blob_free(blob_owner_id(blob)), FMF_OK);
    destroy(h);
}

#[test]
fn last_error_requires_len_pointer() {
    let mut buf = [0u8; 8];
    assert_eq!(
        unsafe { fmf_last_error(buf.as_mut_ptr(), ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
}

// ── 3a. fmf_last_error probe/copy roundtrip ─────────────────────────────

#[test]
fn last_error_probe_rejects_short_buffers_and_success_clears_stale_detail() {
    // LAST_ERROR is thread-local: trigger and read on this same thread.
    // "null string argument" is the known message for a null config.
    let mut out: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(ptr::null(), &raw mut out) },
        FMF_E_INVALID_ARG
    );

    // Probe ignores the input capacity and reports payload bytes (NUL excluded).
    let mut required: u32 = 0;
    assert_eq!(
        unsafe { fmf_last_error(ptr::null_mut(), &raw mut required) },
        FMF_OK
    );
    assert_eq!(required as usize, "null string argument".len());

    // Exactly payload bytes is too small because a real buffer must also
    // hold the terminator. No partial/truncated diagnostic is returned.
    let mut small = vec![0xAAu8; required as usize];
    let mut capacity = required;
    assert_eq!(
        unsafe { fmf_last_error(small.as_mut_ptr(), &raw mut capacity) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(capacity, required);
    assert!(small.iter().all(|&b| b == 0xAA));

    let mut full = vec![0xAAu8; required as usize + 1];
    let mut full_capacity = full.len() as u32;
    assert_eq!(
        unsafe { fmf_last_error(full.as_mut_ptr(), &raw mut full_capacity) },
        FMF_OK
    );
    assert_eq!(full_capacity, required);
    assert_eq!(&full[..required as usize], b"null string argument");
    assert_eq!(full[required as usize], 0);

    // Any later guarded success clears the per-thread detail instead of
    // letting an unrelated caller read this stale error.
    assert_eq!(unsafe { fmf_result_free(ptr::null_mut()) }, FMF_OK);
    let mut empty_required = u32::MAX;
    assert_eq!(
        unsafe { fmf_last_error(ptr::null_mut(), &raw mut empty_required) },
        FMF_OK
    );
    assert_eq!(empty_required, 0);
    let mut empty = [0xAAu8; 1];
    let mut empty_capacity = 1;
    assert_eq!(
        unsafe { fmf_last_error(empty.as_mut_ptr(), &raw mut empty_capacity) },
        FMF_OK
    );
    assert_eq!(empty_capacity, 0);
    assert_eq!(empty[0], 0);
}

// ── 3b. Query syntax-error path (unelevated: no volume involved) ────────

#[test]
fn query_syntax_error_reports_cause_chain() {
    let (h, _dir) = create_engine();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = 0;

    // Parse-stage error: unclosed quote.
    let q = CString::new("\"abc").unwrap();
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_QUERY_SYNTAX
    );
    assert!(rh.is_null(), "no result handle on syntax error");
    let msg = read_last_error();
    assert!(msg.contains("query parse"), "missing stage: {msg}");
    assert!(msg.contains("caused by"), "missing cause chain: {msg}");
    assert!(msg.contains("unclosed quote"), "missing root cause: {msg}");

    // Parse-stage error: bad size filter value.
    let q = CString::new("size:abc").unwrap();
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_QUERY_SYNTAX
    );
    assert!(read_last_error().contains("invalid size filter"));

    // Compile-stage error (bad regex) maps to the same code.
    let q = CString::new("regex:[").unwrap();
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_QUERY_SYNTAX
    );
    let msg = read_last_error();
    assert!(msg.contains("query compile"), "missing stage: {msg}");
    assert!(msg.contains("caused by"), "missing cause chain: {msg}");

    destroy(h);
}

#[test]
fn query_rejects_unknown_enums_non_booleans_and_reserved_bits() {
    let (h, _dir) = create_engine();
    let query = c"foo";
    let invalid = [
        (
            "sort",
            FmfQueryOptions {
                sort: 3,
                ..default_opts()
            },
        ),
        (
            "desc",
            FmfQueryOptions {
                desc: 2,
                ..default_opts()
            },
        ),
        (
            "case_mode",
            FmfQueryOptions {
                case_mode: 3,
                ..default_opts()
            },
        ),
        (
            "include_hidden_system",
            FmfQueryOptions {
                include_hidden_system: 2,
                ..default_opts()
            },
        ),
        (
            "regex_mode",
            FmfQueryOptions {
                regex_mode: 4,
                ..default_opts()
            },
        ),
    ];

    for (field, options) in invalid {
        let mut result_handle = std::ptr::without_provenance_mut::<c_void>(999);
        let mut count = u64::MAX;
        assert_eq!(
            unsafe {
                fmf_query(
                    h,
                    query.as_ptr(),
                    &raw const options,
                    &raw mut result_handle,
                    &raw mut count,
                    ptr::null_mut(),
                )
            },
            FMF_E_INVALID_ARG
        );
        assert!(result_handle.is_null());
        assert_eq!(count, 0);
        assert!(read_last_error().contains(field));
    }
    destroy(h);
}

#[test]
fn valid_query_on_volumeless_engine_succeeds_empty() {
    // Contract: queries succeed against "Ready volumes only" — zero Ready
    // volumes is an empty result, not an error.
    let (h, _dir) = create_engine();
    let q = CString::new("foo").unwrap();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = u64::MAX;
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_OK
    );
    assert_eq!(count, 0);
    assert!(!rh.is_null(), "an empty result still yields a handle");

    let mut page: *mut FmfPage = ptr::null_mut();
    // result_page null matrix needs a live handle, so it lives here.
    assert_eq!(
        unsafe { fmf_result_page(ptr::null_mut(), 0, 1, &raw mut page) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_result_page(rh, 0, 1, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(unsafe { fmf_result_page(rh, 0, 16, &raw mut page) }, FMF_OK);
    assert_eq!(unsafe { (*page).row_count }, 0);
    assert_eq!(fmf_page_free(page_owner_id(page)), FMF_OK);
    assert_eq!(unsafe { fmf_result_free(rh) }, FMF_OK);
    destroy(h);
}

#[test]
fn freed_result_ids_reject_double_free_while_admitted_arc_stays_safe() {
    let (h, _dir) = create_engine();
    let options = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count = u64::MAX;
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                c"foo".as_ptr(),
                &raw const options,
                &raw mut rh,
                &raw mut count,
                ptr::null_mut(),
            )
        },
        FMF_OK
    );
    let admitted = result(rh).expect("registered result");
    assert_eq!(unsafe { fmf_result_free(rh) }, FMF_OK);
    assert_eq!(admitted.len(), 0, "in-flight Arc remains usable after free");

    let mut page: *mut FmfPage = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_result_page(rh, 0, 1, &raw mut page) },
        FMF_E_INVALID_ARG
    );
    assert!(page.is_null());
    assert!(read_last_error().contains("freed result handle"));
    assert_eq!(unsafe { fmf_result_free(rh) }, FMF_E_INVALID_ARG);
    assert!(read_last_error().contains("already freed"));

    destroy(h);
}

// ── 3c. Page/blob packing roundtrip ─────────────────────────────────────

#[test]
fn page_packs_rows_and_string_blob_per_contract() {
    let (h, _dir) = ready_engine();
    let q = CString::new("alpha").unwrap();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = 0;
    let mut trace: *mut FmfBlob = ptr::null_mut();
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                &raw const opts,
                &raw mut rh,
                &raw mut count,
                &raw mut trace,
            )
        },
        FMF_OK
    );
    assert_eq!(count, 1);

    // out_trace (nullable, requested here): QueryTrace as UTF-8 JSON.
    let tjson = json_from_blob(trace);
    assert_eq!(tjson["query_length"], 5);
    assert!(
        tjson.get("query").is_none(),
        "diagnostics must never expose raw query text"
    );
    assert_eq!(fmf_blob_free(blob_owner_id(trace)), FMF_OK);

    // One contiguous block: row header array + string blob, offsets into it.
    let mut page: *mut FmfPage = ptr::null_mut();
    assert_eq!(unsafe { fmf_result_page(rh, 0, 16, &raw mut page) }, FMF_OK);
    let p = unsafe { page.as_ref() }.expect("page is non-null");
    assert_eq!(p.row_count, 1);
    assert!(!p.rows.is_null());
    assert!(!p.blob.is_null());
    assert_eq!(p.blob_len as usize, "alpha.txt".len() + "C:\\".len());

    let row: &FmfRow = unsafe { p.rows.as_ref() }.expect("rows pointer is non-null");
    assert_eq!(row.entry_ref >> 32, 0, "volume ordinal in the high half");
    assert_eq!(row.frn, (1 << 48) | 0x64);
    assert_eq!(row.size, 1234);
    assert_eq!(row.mtime, MT_ALPHA);
    let name = unsafe {
        std::slice::from_raw_parts(p.blob.add(row.name_off as usize), row.name_len as usize)
    };
    assert_eq!(name, b"alpha.txt");
    let parent = unsafe {
        std::slice::from_raw_parts(
            p.blob.add(row.parent_path_off as usize),
            row.parent_path_len as usize,
        )
    };
    assert_eq!(parent, b"C:\\");
    assert_eq!(fmf_page_free(page_owner_id(page)), FMF_OK);

    // Out-of-range offsets page as empty, not as an error.
    let mut tail: *mut FmfPage = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_result_page(rh, 999, 16, &raw mut tail) },
        FMF_OK
    );
    assert_eq!(unsafe { (*tail).row_count }, 0);
    assert_eq!(fmf_page_free(page_owner_id(tail)), FMF_OK);

    assert_eq!(unsafe { fmf_result_free(rh) }, FMF_OK);
    destroy(h);
}
