//! FFI contract tests — literal pins on the values `fmf-contract` defines.
//!
//! These deliberately repeat the numbers instead of importing them: an
//! independent tripwire only works if it does not read from the source it is
//! guarding. A miss-edit of `fmf-contract` fails here.
//!
//! Five families:
//! 1. **ABI layout pins**: struct sizes/offsets that the C# marshaling layer
//!    (`LayoutKind.Explicit` mirrors) depends on. `FmfRow` = 56 bytes is
//!    contractual; for the other structs only the FFI side observes the byte
//!    layout, so the *current* layout is pinned here as a regression detector
//!    — any drift must be a conscious ABI-version-bumping change.
//! 2. **Null/invalid-argument matrix**: every export's `FMF_E_INVALID_ARG`
//!    paths, plus the "null is OK" contract of the free functions.
//! 3. **Behavior roundtrips**: strict `fmf_last_error` probe/copy, the query
//!    syntax-error cause chain, and page/blob packing.
//! 4. **Panic firewall**: this crate's reason to exist. `guard`/`guard_cleanup`
//!    turn an unwind into `FMF_E_PANIC`, and *every* entry point must be
//!    wrapped — one gap turns a recoverable bug into a host-process abort.
//!    Proved on three independent legs (behavioral, structural, per-export).
//! 5. **Presentation basis**: ADR-0044's ownership rule, that only a live,
//!    same-engine basis handle may authorize `QueryTrace.unchanged`.
//!
//! Everything here runs unelevated: `fmf_index_start` is never pointed at a
//! real volume; ready volumes are injected via `Engine::insert_ready_volume`.

use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::fs;
use std::mem::offset_of;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

use fmf_core::engine::Engine;
use fmf_core::index::testutil::TestDir;

use crate::blob::{FmfBlob, fmf_blob_free, fmf_engine_stats};
use crate::error::{fmf_last_error, guard, guard_cleanup, set_error};
use crate::events::{
    CallbackSink, FMF_EVENT_ENGINE_ERROR, FMF_EVENT_INDEX_CHANGED, FMF_EVENT_PROGRESS,
    FMF_EVENT_RESCAN_STARTED, FMF_EVENT_VOLUME_FAILED, FMF_EVENT_VOLUME_READY, FmfEvent,
    FmfEventCb, fmf_set_event_callback,
};
use crate::handle::{engine, fmf_abi_version, fmf_engine_create, fmf_engine_destroy, fmf_flush};
use crate::query_control::{
    fmf_query_control_cancel, fmf_query_control_create, fmf_query_control_free, register_for_test,
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
    assert_eq!(fmf_engine_destroy(h), FMF_OK);
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
    let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
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
    // Shared with the pipe protocol; append-only, and renumbering is a
    // breaking protocol change.
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
    assert_eq!(fmf_contract::options::RegexScope::Name as u32, 0);
    assert_eq!(fmf_contract::options::RegexScope::Path as u32, 1);
    assert_eq!(fmf_contract::options::VolumeState::Scanning as u32, 0);
    assert_eq!(fmf_contract::options::VolumeState::Ready as u32, 1);
    assert_eq!(fmf_contract::options::VolumeState::Rescanning as u32, 2);
    assert_eq!(fmf_contract::options::VolumeState::Failed as u32, 3);
    assert_eq!(fmf_contract::events::SEVERITY_WARN, 1);
    assert_eq!(fmf_contract::events::SEVERITY_ERROR, 2);
    assert_eq!(fmf_contract::events::SEVERITY_PANIC, 3);
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
    assert_eq!(fmf_result_free(ptr::null_mut()), FMF_OK);
    // fmf_engine_destroy is not free-like: a null handle is an error.
    assert_eq!(fmf_engine_destroy(ptr::null_mut()), FMF_E_INVALID_ARG);
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
    assert_eq!(fmf_engine_destroy(h), FMF_E_INVALID_ARG);
    assert!(read_last_error().contains("already destroyed"));
}

#[test]
fn destroy_sweeps_controls_inserted_by_already_admitted_calls() {
    let (h, _dir) = create_engine();
    let admitted = engine(h).expect("registered engine");
    let active = admitted.enter().expect("call admitted before destroy");
    let raw_handle = h as usize;

    let destroy_thread = std::thread::spawn(move || fmf_engine_destroy(raw_handle as *mut c_void));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while admitted.is_accepting_for_test() {
        assert!(
            std::time::Instant::now() < deadline,
            "destroy did not close admission"
        );
        std::thread::yield_now();
    }

    // Emulate the tail of `fmf_query_control_create`: it entered before
    // destruction, but its registry insertion occurs after destroy's initial
    // cancellation sweep and while the lifecycle barrier waits for this call.
    let control_id = register_for_test(admitted.id).expect("late admitted control");
    drop(active);
    assert_eq!(destroy_thread.join().expect("destroy thread"), FMF_OK);
    assert_eq!(
        fmf_query_control_cancel(control_id),
        FMF_E_INVALID_ARG,
        "destroy's post-barrier sweep must remove the late control"
    );
}

#[test]
fn flush_null_matrix_and_roundtrip() {
    assert_eq!(fmf_flush(ptr::null_mut()), FMF_E_INVALID_ARG);
    // Roundtrip on an injected Ready volume: flush succeeds and writes the
    // snapshot file the engine layer is contracted to produce.
    let (h, _dir) = ready_engine();
    assert_eq!(fmf_flush(h), FMF_OK);
    // Second flush is also FMF_OK — "nothing dirty" is success, not an error.
    assert_eq!(fmf_flush(h), FMF_OK);
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
    assert_eq!(count, 0, "invalid handles still clear the volume count");
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
    assert_eq!(count, 0, "invalid handles still clear the status count");
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
    rh = usize::MAX as *mut c_void;
    count = u64::MAX;
    let mut trace = usize::MAX as *mut FmfBlob;
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                ptr::null(),
                &raw mut rh,
                &raw mut count,
                &raw mut trace,
            )
        },
        FMF_E_INVALID_ARG
    );
    assert!(
        rh.is_null(),
        "invalid options still clear the result handle"
    );
    assert_eq!(count, 0, "invalid options still clear the result count");
    assert!(trace.is_null(), "invalid options still clear the trace");
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
    let mut blob = usize::MAX as *mut FmfBlob;
    assert_eq!(
        unsafe { fmf_engine_stats(ptr::null_mut(), &raw mut blob) },
        FMF_E_INVALID_ARG
    );
    assert!(
        blob.is_null(),
        "invalid handles still clear the stats output"
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
    assert_eq!(fmf_result_free(ptr::null_mut()), FMF_OK);
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
    assert_eq!(fmf_result_free(rh), FMF_OK);
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
    assert_eq!(fmf_result_free(rh), FMF_OK);
    assert_eq!(admitted.len(), 0, "in-flight Arc remains usable after free");

    let mut page: *mut FmfPage = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_result_page(rh, 0, 1, &raw mut page) },
        FMF_E_INVALID_ARG
    );
    assert!(page.is_null());
    assert!(read_last_error().contains("freed result handle"));
    assert_eq!(fmf_result_free(rh), FMF_E_INVALID_ARG);
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

    assert_eq!(fmf_result_free(rh), FMF_OK);
    destroy(h);
}

// ── 4. Panic firewall: catch_unwind on every entry point ────────────────
//
// A panic that leaves an `extern "C"` frame is not a recoverable error: the
// Rust ABI shim aborts the process, taking the WinUI host down with it.
// `guard` is the only thing standing between a fmf-core bug and that abort, so
// the invariant is not "the guard works" but "*every* door has one". Three
// independent legs prove it:
//
// * `guard`/`guard_cleanup` themselves convert an unwind into `FMF_E_PANIC`
//   with the contracted detail text (the two `guard_*` tests below);
// * every export is *observed at run time* to execute the guard prologue
//   (`every_export_actually_enters_its_guard`); and
// * the committed source of every `extern "C" fn` opens and closes its body
//   with that wrapper (`every_extern_c_entry_point_is_wrapped_in_the_guard`),
//   which is the only leg that can fail for an entry point nobody thought to
//   call from a test.

/// Serializes the two tests that install a process-global panic hook.
static PANIC_HOOK: Mutex<()> = Mutex::new(());

/// Runs `f` with panic output suppressed, so a deliberately panicking guard
/// body does not spray a backtrace over the test log.
fn without_panic_noise<R>(f: impl FnOnce() -> R) -> R {
    let _serialize = PANIC_HOOK.lock().expect("panic-hook lock");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = f();
    std::panic::set_hook(previous);
    outcome
}

#[test]
fn guard_converts_any_unwind_into_the_panic_code_and_detail() {
    let (from_string, from_other) = without_panic_noise(|| {
        let string_code = guard(|| panic!("engine invariant violated"));
        let string_detail = read_last_error();
        // A non-`&str` payload must not be handled differently: the boundary
        // reports the same code and the same text either way.
        let other_code = guard(|| std::panic::panic_any(7_u32));
        let other_detail = read_last_error();
        ((string_code, string_detail), (other_code, other_detail))
    });
    assert_eq!(
        from_string,
        (FMF_E_PANIC, "panic inside fmf_engine".to_owned())
    );
    assert_eq!(
        from_other,
        (FMF_E_PANIC, "panic inside fmf_engine".to_owned())
    );

    // The panic detail belongs to that one call: the next guarded success
    // clears it instead of letting an unrelated caller read it back.
    assert_eq!(guard(|| FMF_OK), FMF_OK);
    assert_eq!(read_last_error(), "");
}

#[test]
fn guard_cleanup_restores_prior_detail_only_when_the_cleanup_itself_succeeded() {
    // Success: the query's diagnostic survives its mandatory cleanup call.
    set_error("prior operation detail");
    assert_eq!(guard_cleanup(|| FMF_OK), FMF_OK);
    assert_eq!(read_last_error(), "prior operation detail");

    // Failure: the cleanup's own error is the richer one and replaces it.
    set_error("prior operation detail");
    assert_eq!(
        guard_cleanup(|| {
            set_error("cleanup failed");
            FMF_E_IO
        }),
        FMF_E_IO
    );
    assert_eq!(read_last_error(), "cleanup failed");

    // Panic: not a success, so the restore must not run — reporting the prior
    // operation's text here would hide a crash behind an unrelated message.
    set_error("prior operation detail");
    let code = without_panic_noise(|| guard_cleanup(|| panic!("cleanup exploded")));
    assert_eq!(code, FMF_E_PANIC);
    assert_eq!(read_last_error(), "panic inside fmf_engine");
}

/// A distinctive per-thread detail seeded before an entry point is called.
const GUARD_SENTINEL: &str = "sentinel left by an earlier call on this thread";

/// Asserts that `call` — a *successful* invocation of a real entry point —
/// ran the `guard` prologue.
///
/// `guard` clears the thread-local detail before running the body, so if the
/// sentinel is still readable after a call that returned `FMF_OK`, nothing in
/// that entry point cleared it: it never entered a guard, and therefore has no
/// `catch_unwind` either. That makes this a run-time observation of the
/// firewall's *presence at each door*, not merely of its implementation.
fn assert_enters_guard(name: &str, call: impl FnOnce() -> i32) {
    set_error(GUARD_SENTINEL);
    assert_eq!(call(), FMF_OK, "{name} must take its success path here");
    assert_ne!(
        read_last_error(),
        GUARD_SENTINEL,
        "{name} returned without clearing the thread-local detail: it is not wrapped in guard()"
    );
}

#[test]
fn every_export_actually_enters_its_guard() {
    let dir = TestDir::new();
    let cfg = CString::new(
        serde_json::json!({
            "index_dir": dir.join("index").to_string_lossy(),
            "log_dir": dir.join("logs").to_string_lossy(),
            "log_level": "warn",
        })
        .to_string(),
    )
    .unwrap();

    let mut h: *mut c_void = ptr::null_mut();
    assert_enters_guard("fmf_engine_create", || unsafe {
        fmf_engine_create(cfg.as_ptr(), &raw mut h)
    });
    assert_enters_guard("fmf_set_event_callback", || unsafe {
        fmf_set_event_callback(h, Some(noop_event_cb), ptr::null_mut())
    });

    let mut count = 0u32;
    assert_enters_guard("fmf_list_volumes", || unsafe {
        fmf_list_volumes(h, ptr::null_mut(), 0, &raw mut count)
    });
    assert_enters_guard("fmf_index_start", || unsafe {
        fmf_index_start(h, ptr::null(), 0)
    });
    assert_enters_guard("fmf_index_status", || unsafe {
        fmf_index_status(h, ptr::null_mut(), 0, &raw mut count)
    });

    let mut blob: *mut FmfBlob = ptr::null_mut();
    assert_enters_guard("fmf_engine_stats", || unsafe {
        fmf_engine_stats(h, &raw mut blob)
    });
    let stats_owner = blob_owner_id(blob);
    assert_enters_guard("fmf_blob_free", || fmf_blob_free(stats_owner));

    let mut control_id = 0u64;
    assert_enters_guard("fmf_query_control_create", || unsafe {
        fmf_query_control_create(h, &raw mut control_id)
    });

    let query = CString::new("foo").unwrap();
    let options = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut hits = 0u64;
    assert_enters_guard("fmf_query", || unsafe {
        raw_fmf_query(
            h,
            query.as_ptr(),
            &raw const options,
            control_id,
            &raw mut rh,
            &raw mut hits,
            ptr::null_mut(),
        )
    });

    let mut page: *mut FmfPage = ptr::null_mut();
    assert_enters_guard("fmf_result_page", || unsafe {
        fmf_result_page(rh, 0, 16, &raw mut page)
    });
    let page_owner = page_owner_id(page);
    assert_enters_guard("fmf_page_free", || fmf_page_free(page_owner));
    assert_enters_guard("fmf_result_free", || fmf_result_free(rh));
    assert_enters_guard("fmf_query_control_cancel", || {
        fmf_query_control_cancel(control_id)
    });

    // The one deliberate asymmetry: `fmf_query_control_free` is the mandatory
    // cleanup for `fmf_query`, so on success `guard_cleanup` puts the previous
    // detail back. The guard still ran — the restore is itself the proof,
    // since only `guard_cleanup` can reinstate a cleared message.
    set_error(GUARD_SENTINEL);
    assert_eq!(fmf_query_control_free(control_id), FMF_OK);
    assert_eq!(
        read_last_error(),
        GUARD_SENTINEL,
        "fmf_query_control_free must preserve the preceding operation's diagnostic"
    );

    assert_enters_guard("fmf_flush", || fmf_flush(h));
    assert_enters_guard("fmf_engine_destroy", || fmf_engine_destroy(h));

    // …and the two documented exemptions behave as documented.
    set_error(GUARD_SENTINEL);
    assert_eq!(fmf_abi_version(), 5);
    assert_eq!(
        read_last_error(),
        GUARD_SENTINEL,
        "fmf_abi_version is a const entry point with no guard; it must not disturb the detail"
    );
    // fmf_last_error is exercised by every `read_last_error()` above: guarding
    // it would clear the very buffer it exists to hand back.
}

/// Production modules of this crate, by name, with their committed text.
///
/// `contract_tests.rs` is deliberately absent: it is `#[cfg(test)]`, never
/// linked into the cdylib, and its own `extern "C"` items are stand-ins for
/// *foreign* callbacks (which the contract requires not to unwind), not
/// exports. `no_ffi_module_escapes_the_guard_audit` proves the list is total.
const FFI_MODULES: &[(&str, &str)] = &[
    ("allocation.rs", include_str!("allocation.rs")),
    ("blob.rs", include_str!("blob.rs")),
    ("error.rs", include_str!("error.rs")),
    ("events.rs", include_str!("events.rs")),
    ("handle.rs", include_str!("handle.rs")),
    ("lib.rs", include_str!("lib.rs")),
    ("opaque.rs", include_str!("opaque.rs")),
    ("query_control.rs", include_str!("query_control.rs")),
    ("results.rs", include_str!("results.rs")),
    ("volumes.rs", include_str!("volumes.rs")),
];

/// The complete C-ABI surface. Adding an export means adding it here, to
/// `export_pins` in lib.rs, and to the run-time matrix above.
const EXPECTED_EXPORTS: &[&str] = &[
    "fmf_abi_version",
    "fmf_blob_free",
    "fmf_engine_create",
    "fmf_engine_destroy",
    "fmf_engine_stats",
    "fmf_flush",
    "fmf_index_start",
    "fmf_index_status",
    "fmf_last_error",
    "fmf_list_volumes",
    "fmf_page_free",
    "fmf_query",
    "fmf_query_control_cancel",
    "fmf_query_control_create",
    "fmf_query_control_free",
    "fmf_result_free",
    "fmf_result_page",
    "fmf_set_event_callback",
];

/// The only entry points allowed to have no guard, each with the reason no
/// guard is possible. This list is the review checkpoint: a third entry has to
/// be a deliberate decision, never an oversight.
const UNGUARDED_EXPORTS: &[(&str, &str)] = &[
    (
        "fmf_abi_version",
        "a `const extern \"C\" fn` whose body is one compile-time constant: \
         there is nothing to unwind, and `guard` is not callable in const context",
    ),
    (
        "fmf_last_error",
        "`guard`'s first act is clearing LAST_ERROR — the very buffer this \
         function exists to hand back, so wrapping it would always report \"\"",
    ),
];

/// Every `extern "C" fn` *declaration* in `source`, paired with whether the
/// guard wrapper is its entire body.
///
/// Function-pointer *types* (`extern "C" fn(…)`, used for the event callback)
/// are not declarations and are skipped: the keyword below ends in a space, so
/// only a named `fn` matches. The body is located by rustfmt's layout — the
/// signature ends on the first line closing with `{`, and a top-level item's
/// closing brace is the next line that is exactly `}` — which `cargo fmt
/// --check` keeps true as part of the same gate that runs this test.
fn extern_c_declarations(source: &str) -> Vec<(String, bool)> {
    const KEYWORD: &str = "extern \"C\" fn ";
    let lines: Vec<&str> = source.lines().collect();
    let mut declarations = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let Some(offset) = line.find(KEYWORD) else {
            continue;
        };
        let name: String = line[offset + KEYWORD.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assert!(
            !name.is_empty(),
            "unnamed `extern \"C\" fn` declaration on line {}",
            index + 1
        );

        let opens = lines[index..]
            .iter()
            .position(|candidate| candidate.trim_end().ends_with('{'))
            .map_or_else(|| panic!("`{name}` has no body"), |offset| index + offset);
        let closes = lines[opens..]
            .iter()
            .position(|candidate| *candidate == "}")
            .map_or_else(
                || panic!("`{name}` has no top-level closing brace"),
                |offset| opens + offset,
            );

        let first = lines[opens + 1..closes]
            .iter()
            .find(|candidate| !candidate.trim().is_empty())
            .unwrap_or_else(|| panic!("`{name}` has an empty body"));
        let opens_guard = matches!(first.trim(), "guard(|| {" | "guard_cleanup(|| {");
        // The wrapper must also be the *outermost* thing in the body: a guard
        // closed early would leave the remaining statements unprotected.
        let closes_guard = lines[closes - 1].trim() == "})";
        declarations.push((name, opens_guard && closes_guard));
    }
    declarations
}

#[test]
fn no_ffi_module_escapes_the_guard_audit() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut present: Vec<String> = fs::read_dir(&src)
        .expect("fmf-ffi/src must be readable")
        .map(|entry| entry.expect("directory entry must be readable"))
        .map(|entry| {
            assert!(
                entry
                    .file_type()
                    .expect("file type must be readable")
                    .is_file(),
                "fmf-ffi/src is flat; a submodule directory would hide entry points from the audit"
            );
            entry.file_name().to_string_lossy().into_owned()
        })
        // Case-insensitively, because Windows resolves `mod blob;` to a
        // `Blob.RS` just as happily — and that spelling would slip past an
        // exact-suffix filter while still compiling into the cdylib.
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
        })
        .collect();
    present.sort();

    let mut audited: Vec<String> = FFI_MODULES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .chain(std::iter::once("contract_tests.rs".to_owned()))
        .collect();
    audited.sort();

    assert_eq!(
        present, audited,
        "a module was added to or removed from fmf-ffi/src without updating FFI_MODULES; \
         an unlisted module's entry points would never be checked for a panic guard"
    );
}

#[test]
fn every_extern_c_entry_point_is_wrapped_in_the_guard() {
    let mut inventory: BTreeMap<String, (&str, bool)> = BTreeMap::new();
    for (module, source) in FFI_MODULES {
        for (name, guarded) in extern_c_declarations(source) {
            assert!(
                inventory.insert(name.clone(), (*module, guarded)).is_none(),
                "`{name}` is declared twice"
            );
        }
    }

    let found: Vec<&str> = inventory.keys().map(String::as_str).collect();
    let mut expected = EXPECTED_EXPORTS.to_vec();
    expected.sort_unstable();
    assert_eq!(
        found, expected,
        "the C-ABI surface changed; update EXPECTED_EXPORTS, lib.rs `export_pins` \
         and the run-time matrix in `every_export_actually_enters_its_guard`"
    );

    for (name, (module, guarded)) in &inventory {
        match UNGUARDED_EXPORTS.iter().find(|(export, _)| export == name) {
            Some((_, reason)) => assert!(
                !guarded,
                "`{name}` is exempt from the panic guard because {reason} — it now has one, \
                 so delete the exemption instead of leaving a stale excuse in place"
            ),
            None => assert!(
                *guarded,
                "{module}: `{name}` does not wrap its whole body in guard(|| {{ … }}) / \
                 guard_cleanup(|| {{ … }}). An unwind out of an unguarded extern \"C\" frame \
                 aborts the host process instead of returning FMF_E_PANIC"
            ),
        }
    }

    // Tie the discovered surface to the signature pins so the two lists in
    // lib.rs and here cannot drift apart.
    let lib = FFI_MODULES
        .iter()
        .find_map(|(name, source)| (*name == "lib.rs").then_some(*source))
        .expect("lib.rs is audited");
    let pins = lib
        .split_once("mod export_pins")
        .expect("lib.rs declares export_pins")
        .1;
    for name in inventory.keys() {
        assert!(
            pins.contains(name.as_str()),
            "`export_pins` does not pin `{name}`'s name and signature"
        );
    }
}

// ── 5. Presentation-basis ownership (ADR-0044) ──────────────────────────
//
// `QueryTrace.unchanged` is what lets the UI refresh rows in place instead of
// resetting the list. Authorizing it from anything but a live result of *this*
// engine would repaint stale rows (or skip a repaint that was needed), so the
// boundary — not core — has to normalize a foreign basis to "no basis".

fn opts_with_basis(presentation_basis: u64) -> FmfQueryOptions {
    FmfQueryOptions {
        presentation_basis,
        ..default_opts()
    }
}

/// Runs one query with an explicit presentation basis, returning the new
/// result handle, the hit count, and `QueryTrace.unchanged` from the trace.
fn query_with_basis(h: *mut c_void, text: &str, basis: u64) -> (*mut c_void, u64, bool) {
    let query = CString::new(text).unwrap();
    let options = opts_with_basis(basis);
    let mut rh: *mut c_void = ptr::null_mut();
    let mut hits = 0u64;
    let mut trace: *mut FmfBlob = ptr::null_mut();
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                query.as_ptr(),
                &raw const options,
                &raw mut rh,
                &raw mut hits,
                &raw mut trace,
            )
        },
        FMF_OK,
        "a basis is never an argument error, only an authorization: {}",
        read_last_error()
    );
    let unchanged = json_from_blob(trace)["unchanged"]
        .as_bool()
        .expect("QueryTrace carries `unchanged`");
    assert_eq!(fmf_blob_free(blob_owner_id(trace)), FMF_OK);
    (rh, hits, unchanged)
}

#[test]
fn only_a_live_same_engine_basis_authorizes_unchanged() {
    let (h, _dir) = ready_engine();

    let (first, hits, unchanged) = query_with_basis(h, "alpha", 0);
    assert_eq!(hits, 1);
    assert!(!unchanged, "basis 0 makes no identity claim");

    let basis = first.addr() as u64;
    let (second, _, unchanged) = query_with_basis(h, "alpha", basis);
    assert!(
        unchanged,
        "an identical query over a live same-engine basis is the RefreshInPlace case"
    );

    // A different query cannot inherit identity from that basis.
    let (third, hits, unchanged) = query_with_basis(h, "beta", basis);
    assert_eq!(hits, 1);
    assert!(!unchanged, "a different id column must reset the list");

    // Chaining works: the second result is itself a valid basis.
    let (fourth, _, unchanged) = query_with_basis(h, "alpha", second.addr() as u64);
    assert!(unchanged);

    for handle in [first, second, third, fourth] {
        assert_eq!(fmf_result_free(handle), FMF_OK);
    }
    destroy(h);
}

#[test]
fn a_freed_forged_or_cross_kind_basis_is_normalized_to_no_basis() {
    let (h, _dir) = ready_engine();
    let (live, _, _) = query_with_basis(h, "alpha", 0);
    let live_basis = live.addr() as u64;

    // Sanity: this exact value *does* authorize identity while it is live, so
    // every "no" below is caused by the basis rule and not by the query.
    let (baseline, _, unchanged) = query_with_basis(h, "alpha", live_basis);
    assert!(unchanged);
    assert_eq!(fmf_result_free(baseline), FMF_OK);

    assert_eq!(fmf_result_free(live), FMF_OK);
    let rejected = [
        // Freed: the same value was authorizing one call ago.
        ("a freed basis", live_basis),
        // Never issued: opaque ids are monotonic, so this is far ahead of any
        // handle this process will mint during the test.
        ("a never-issued basis", live_basis + 1_000_000),
        ("a saturated basis", u64::MAX),
        // Engine and result handles share one opaque id namespace, so a
        // cross-kind id is a live id that simply is not a result.
        ("an engine handle as basis", h.addr() as u64),
    ];
    for (name, basis) in rejected {
        let (rh, hits, unchanged) = query_with_basis(h, "alpha", basis);
        assert_eq!(hits, 1, "{name} must not disturb the result itself");
        assert!(!unchanged, "{name} must behave exactly as no basis");
        assert_eq!(fmf_result_free(rh), FMF_OK);
    }
    destroy(h);
}

#[test]
fn a_cross_engine_basis_is_normalized_to_no_basis() {
    let (first, _first_dir) = ready_engine();
    let (second, _second_dir) = ready_engine();

    let (foreign, _, _) = query_with_basis(first, "alpha", 0);
    let foreign_basis = foreign.addr() as u64;

    // Both engines hold synthetic volumes with the same label and the same two
    // files, so the *contents* match. Only ownership separates them — which is
    // exactly the confusion this rule exists to prevent.
    let (owned, hits, unchanged) = query_with_basis(second, "alpha", foreign_basis);
    assert_eq!(hits, 1);
    assert!(
        !unchanged,
        "a live result belonging to another engine must never authorize in-place refresh"
    );
    assert_eq!(fmf_result_free(owned), FMF_OK);

    // The rejection is about ownership, not about a second engine existing:
    // the owning engine still accepts its own basis.
    let (still_valid, _, unchanged) = query_with_basis(first, "alpha", foreign_basis);
    assert!(unchanged);
    assert_eq!(fmf_result_free(still_valid), FMF_OK);

    assert_eq!(fmf_result_free(foreign), FMF_OK);
    destroy(first);
    destroy(second);
}

#[test]
fn destroying_an_engine_invalidates_its_results_as_a_basis() {
    let (h, _dir) = ready_engine();
    let (result_handle, _, _) = query_with_basis(h, "alpha", 0);
    let basis = result_handle.addr() as u64;
    destroy(h);

    // `fmf_engine_destroy` purges the engine's results, so a handle that
    // outlived its engine cannot be replayed against a fresh one.
    let (next, _dir_next) = ready_engine();
    let (rh, _, unchanged) = query_with_basis(next, "alpha", basis);
    assert!(!unchanged, "a basis cannot survive its engine");
    assert_eq!(fmf_result_free(rh), FMF_OK);
    assert_eq!(
        fmf_result_free(result_handle),
        FMF_E_INVALID_ARG,
        "the purged handle is gone, not merely unusable as a basis"
    );
    destroy(next);
}
