//! FFI contract tests — docs/ARCHITECTURE.md is the canonical contract.
//!
//! Three families:
//! 1. **ABI layout pins**: struct sizes/offsets that the C# marshaling layer
//!    (`LayoutKind.Sequential` mirrors) depends on. `FmfRow` = 48 bytes is
//!    contractual; for the other structs the contract does not spell out a
//!    byte layout, so the *current* layout is pinned here as a regression
//!    detector — any drift must be a conscious, doc-updating change.
//! 2. **Null/invalid-argument matrix**: every export's `FMF_E_INVALID_ARG`
//!    paths, plus the "null is OK" contract of the free functions.
//! 3. **Behavior roundtrips**: `fmf_last_error` truncation, the query
//!    syntax-error cause chain, and page/blob packing.
//!
//! Everything here runs unelevated: `fmf_index_start` is never pointed at a
//! real volume; ready volumes are injected via `Engine::insert_ready_volume`.

use std::ffi::{CString, c_char, c_void};
use std::mem::offset_of;
use std::ptr;

use crate::blob::{FmfBlob, fmf_blob_free, fmf_engine_stats};
use crate::error::fmf_last_error;
use crate::events::{
    FMF_EVENT_ENGINE_ERROR, FMF_EVENT_INDEX_CHANGED, FMF_EVENT_PROGRESS, FMF_EVENT_RESCAN_STARTED,
    FMF_EVENT_VOLUME_FAILED, FMF_EVENT_VOLUME_READY, FmfEvent, FmfEventCb, fmf_set_event_callback,
};
use crate::handle::{
    EngineHandle, fmf_abi_version, fmf_engine_create, fmf_engine_destroy, fmf_flush,
};
use crate::results::{
    FmfPage, FmfQueryOptions, FmfRow, fmf_page_free, fmf_query, fmf_result_free, fmf_result_page,
};
use crate::volumes::{FmfVolumeStatus, fmf_index_start, fmf_index_status, fmf_list_volumes};
use crate::{
    FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_LOCKED, FMF_E_NOT_ADMIN, FMF_E_PANIC, FMF_E_QUERY_SYNTAX,
    FMF_E_STALE, FMF_E_VOLUME, FMF_OK,
};

// ── helpers ─────────────────────────────────────────────────────────────

/// A per-call unique base dir: the engine's writer lock turns a shared
/// index dir into a cross-test collision under the parallel test runner.
fn unique_test_dir() -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "fmf-ffi-contract-tests-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// Creates an engine against temp directories — no volume is touched, so
/// this needs no elevation (Engine::new only builds in-memory state).
fn create_engine() -> *mut c_void {
    let dir = unique_test_dir();
    let cfg = serde_json::json!({
        "index_dir": dir.join("index").to_string_lossy(),
        "log_dir": dir.join("logs").to_string_lossy(),
        "log_level": "warn",
    })
    .to_string();
    let cfg = CString::new(cfg).unwrap();
    let mut h: *mut c_void = ptr::null_mut();
    let rc = unsafe { fmf_engine_create(cfg.as_ptr(), &mut h) };
    assert_eq!(
        rc,
        FMF_OK,
        "fmf_engine_create failed: {}",
        read_last_error()
    );
    assert!(!h.is_null());
    h
}

fn destroy(h: *mut c_void) {
    assert_eq!(unsafe { fmf_engine_destroy(h) }, FMF_OK);
}

/// Reads the thread-local detail message with an ample buffer.
fn read_last_error() -> String {
    let mut buf = [0u8; 1024];
    let mut len: u32 = buf.len() as u32;
    assert_eq!(
        unsafe { fmf_last_error(buf.as_mut_ptr(), &mut len) },
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
    }
}

fn json_from_blob(blob: *mut FmfBlob) -> serde_json::Value {
    assert!(!blob.is_null());
    let b = unsafe { &*blob };
    let bytes = unsafe { std::slice::from_raw_parts(b.data, b.len as usize) };
    serde_json::from_slice(bytes).expect("blob is UTF-8 JSON")
}

/// Engine with one injected Ready volume ("C:", two files) — the unelevated
/// stand-in for a real MFT scan.
fn ready_engine() -> *mut c_void {
    use fmf_core::index::{RawEntry, VolumeIndexBuilder};

    let h = create_engine();
    let mut b = VolumeIndexBuilder::new("C:", 5);
    let alpha: Vec<u16> = "alpha.txt".encode_utf16().collect();
    b.push(RawEntry {
        record: 100,
        parent_record: 5,
        frn: (1 << 48) | 100,
        name_utf16: &alpha,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 1234,
        mtime: 777,
    });
    let beta: Vec<u16> = "beta.log".encode_utf16().collect();
    b.push(RawEntry {
        record: 101,
        parent_record: 5,
        frn: (1 << 48) | 101,
        name_utf16: &beta,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 99,
        mtime: 888,
    });
    // The handle struct is crate-visible, so tests can reach the engine
    // behind the opaque pointer without an extra FFI test hook.
    let handle = unsafe { &*h.cast::<EngineHandle>() };
    handle.engine.insert_ready_volume("C:", b.finish());
    h
}

unsafe extern "C" fn noop_event_cb(_ev: *const FmfEvent, _user: *mut c_void) {}

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
    assert_eq!(FMF_E_PANIC, 99);
}

#[test]
fn abi_version_is_pinned() {
    assert_eq!(fmf_abi_version(), 1);
}

#[test]
fn fmf_row_layout_is_48_bytes_no_padding() {
    // Contractual: "48-byte row, no internal padding", mirrored by C#
    // LayoutKind.Sequential. Note the field *order* here (the implemented
    // ABI) differs from the prose order in ARCHITECTURE.md §ページ取得,
    // which would introduce padding; this layout is what C# marshals against.
    assert_eq!(size_of::<FmfRow>(), 48);
    assert_eq!(align_of::<FmfRow>(), 8);
    assert_eq!(offset_of!(FmfRow, entry_ref), 0);
    assert_eq!(offset_of!(FmfRow, frn), 8);
    assert_eq!(offset_of!(FmfRow, size), 16);
    assert_eq!(offset_of!(FmfRow, mtime), 24);
    assert_eq!(offset_of!(FmfRow, name_off), 32);
    assert_eq!(offset_of!(FmfRow, parent_path_off), 36);
    assert_eq!(offset_of!(FmfRow, flags), 40);
    assert_eq!(offset_of!(FmfRow, name_len), 44);
    assert_eq!(offset_of!(FmfRow, parent_path_len), 46);
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
    assert_eq!(size_of::<FmfPage>(), 32);
    assert_eq!(align_of::<FmfPage>(), 8);
    assert_eq!(offset_of!(FmfPage, row_count), 0);
    assert_eq!(offset_of!(FmfPage, _pad), 4);
    assert_eq!(offset_of!(FmfPage, rows), 8);
    assert_eq!(offset_of!(FmfPage, blob), 16);
    assert_eq!(offset_of!(FmfPage, blob_len), 24);
    assert_eq!(offset_of!(FmfPage, _pad2), 28);
}

#[test]
fn fmf_query_options_layout_pinned() {
    // Contract lists the option fields but not a byte layout — pinned.
    assert_eq!(size_of::<FmfQueryOptions>(), 16);
    assert_eq!(align_of::<FmfQueryOptions>(), 4);
    assert_eq!(offset_of!(FmfQueryOptions, sort), 0);
    assert_eq!(offset_of!(FmfQueryOptions, desc), 4);
    assert_eq!(offset_of!(FmfQueryOptions, case_mode), 8);
    assert_eq!(offset_of!(FmfQueryOptions, include_hidden_system), 12);
}

#[test]
fn fmf_blob_layout_pinned() {
    // Contract: { data: *const u8, len: u32 }; trailing pad pinned.
    assert_eq!(size_of::<FmfBlob>(), 16);
    assert_eq!(align_of::<FmfBlob>(), 8);
    assert_eq!(offset_of!(FmfBlob, data), 0);
    assert_eq!(offset_of!(FmfBlob, len), 8);
    assert_eq!(offset_of!(FmfBlob, _pad), 12);
}

// ── 2. Null/invalid-argument matrix ─────────────────────────────────────

#[test]
fn engine_create_rejects_bad_args() {
    let mut out: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(ptr::null(), &mut out) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_engine_create(c"{}".as_ptr(), ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    let bad_utf8: [u8; 2] = [0xFF, 0x00];
    assert_eq!(
        unsafe { fmf_engine_create(bad_utf8.as_ptr().cast::<c_char>(), &mut out) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_engine_create(c"not json".as_ptr(), &mut out) },
        FMF_E_INVALID_ARG
    );
    // index_dir is a required config key.
    assert_eq!(
        unsafe { fmf_engine_create(c"{}".as_ptr(), &mut out) },
        FMF_E_INVALID_ARG
    );
    assert!(read_last_error().contains("index_dir"));
    assert!(out.is_null(), "out must stay untouched on failure");
}

#[test]
fn null_is_ok_for_frees_but_not_destroy() {
    assert_eq!(unsafe { fmf_blob_free(ptr::null_mut()) }, FMF_OK);
    assert_eq!(unsafe { fmf_page_free(ptr::null_mut()) }, FMF_OK);
    assert_eq!(unsafe { fmf_result_free(ptr::null_mut()) }, FMF_OK);
    // fmf_engine_destroy is not free-like: a null handle is an error.
    assert_eq!(
        unsafe { fmf_engine_destroy(ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
}

#[test]
fn flush_null_matrix_and_roundtrip() {
    assert_eq!(unsafe { fmf_flush(ptr::null_mut()) }, FMF_E_INVALID_ARG);
    // Roundtrip on an injected Ready volume: flush succeeds and writes the
    // snapshot file the engine layer is contracted to produce.
    let h = ready_engine();
    assert_eq!(unsafe { fmf_flush(h) }, FMF_OK);
    // Second flush is also FMF_OK — "nothing dirty" is success, not an error.
    assert_eq!(unsafe { fmf_flush(h) }, FMF_OK);
    destroy(h);
}

#[test]
fn second_engine_on_same_index_dir_reports_locked() {
    let dir = unique_test_dir();
    let cfg = serde_json::json!({
        "index_dir": dir.join("index").to_string_lossy(),
        "log_dir": dir.join("logs").to_string_lossy(),
        "log_level": "warn",
    })
    .to_string();
    let cfg = CString::new(cfg).unwrap();

    let mut first: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(cfg.as_ptr(), &mut first) },
        FMF_OK
    );

    let mut second: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(cfg.as_ptr(), &mut second) },
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
        unsafe { fmf_engine_create(cfg.as_ptr(), &mut third) },
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
    let h = create_engine();
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
fn list_volumes_requires_count_only() {
    assert_eq!(
        unsafe { fmf_list_volumes(ptr::null_mut(), ptr::null_mut(), 0, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    // Pin of current behavior: the handle parameter is unused (the volume
    // list is process-global), so even a null handle succeeds. If handle
    // validation is ever added, update this pin and ARCHITECTURE.md together.
    let mut count = u32::MAX;
    assert_eq!(
        unsafe { fmf_list_volumes(ptr::null_mut(), ptr::null_mut(), 0, &mut count) },
        FMF_OK
    );
    assert_ne!(count, u32::MAX, "count must be written");
}

#[test]
fn index_start_null_matrix() {
    assert_eq!(
        unsafe { fmf_index_start(ptr::null_mut(), ptr::null(), 0) },
        FMF_E_INVALID_ARG
    );
    let h = create_engine();
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
    destroy(h);
}

#[test]
fn index_status_null_matrix() {
    let mut count = u32::MAX;
    assert_eq!(
        unsafe { fmf_index_status(ptr::null_mut(), ptr::null_mut(), 0, &mut count) },
        FMF_E_INVALID_ARG
    );
    let h = create_engine();
    assert_eq!(
        unsafe { fmf_index_status(h, ptr::null_mut(), 0, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    // count alone (buf = NULL) is the size-probe pattern.
    assert_eq!(
        unsafe { fmf_index_status(h, ptr::null_mut(), 0, &mut count) },
        FMF_OK
    );
    assert_eq!(count, 0, "no volumes were registered");
    destroy(h);
}

#[test]
fn query_null_matrix() {
    let h = create_engine();
    let q = CString::new("foo").unwrap();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = 0;

    assert_eq!(
        unsafe {
            fmf_query(
                ptr::null_mut(),
                q.as_ptr(),
                &opts,
                &mut rh,
                &mut count,
                ptr::null_mut(),
            )
        },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_query(h, ptr::null(), &opts, &mut rh, &mut count, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe {
            fmf_query(
                h,
                q.as_ptr(),
                ptr::null(),
                &mut rh,
                &mut count,
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
                &opts,
                ptr::null_mut(),
                &mut count,
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
                &opts,
                &mut rh,
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
                &opts,
                &mut rh,
                &mut count,
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
        unsafe { fmf_engine_stats(ptr::null_mut(), &mut blob) },
        FMF_E_INVALID_ARG
    );
    let h = create_engine();
    assert_eq!(
        unsafe { fmf_engine_stats(h, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(unsafe { fmf_engine_stats(h, &mut blob) }, FMF_OK);
    // Contract: engine-allocated UTF-8 JSON, released with fmf_blob_free.
    assert!(json_from_blob(blob).is_object());
    assert_eq!(unsafe { fmf_blob_free(blob) }, FMF_OK);
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

// ── 3a. fmf_last_error truncation roundtrip ─────────────────────────────

#[test]
fn last_error_truncation_roundtrip() {
    // LAST_ERROR is thread-local: trigger and read on this same thread.
    // "null string argument" is the known message for a null config.
    let mut out: *mut c_void = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_engine_create(ptr::null(), &mut out) },
        FMF_E_INVALID_ARG
    );

    // Full read: len is in/out (capacity in, payload bytes out, excluding
    // the NUL appended when room allows).
    let mut buf = [0xAAu8; 64];
    let mut len: u32 = buf.len() as u32;
    assert_eq!(
        unsafe { fmf_last_error(buf.as_mut_ptr(), &mut len) },
        FMF_OK
    );
    let full = std::str::from_utf8(&buf[..len as usize])
        .unwrap()
        .to_string();
    assert_eq!(full, "null string argument");
    assert_eq!(buf[len as usize], 0, "NUL appended after the payload");

    // Truncated: capacity 8 → 7 payload bytes + NUL, len reports 7.
    let mut small = [0xAAu8; 8];
    let mut slen: u32 = small.len() as u32;
    assert_eq!(
        unsafe { fmf_last_error(small.as_mut_ptr(), &mut slen) },
        FMF_OK
    );
    assert_eq!(slen, 7);
    assert_eq!(&small[..7], &full.as_bytes()[..7]);
    assert_eq!(small[7], 0, "truncated copy is still NUL-terminated");

    // Capacity 1: room for the NUL only.
    let mut one = [0xAAu8; 1];
    let mut olen: u32 = 1;
    assert_eq!(
        unsafe { fmf_last_error(one.as_mut_ptr(), &mut olen) },
        FMF_OK
    );
    assert_eq!(olen, 0);
    assert_eq!(one[0], 0);

    // Capacity 0: nothing is written at all (not even a NUL).
    let mut zero = [0xAAu8; 4];
    let mut zlen: u32 = 0;
    assert_eq!(
        unsafe { fmf_last_error(zero.as_mut_ptr(), &mut zlen) },
        FMF_OK
    );
    assert_eq!(zlen, 0);
    assert_eq!(zero, [0xAA; 4], "capacity 0 must not touch the buffer");

    // Size probe: NULL buffer + huge capacity reports the payload length.
    let mut probe: u32 = u32::MAX;
    assert_eq!(
        unsafe { fmf_last_error(ptr::null_mut(), &mut probe) },
        FMF_OK
    );
    assert_eq!(probe as usize, full.len());
}

// ── 3b. Query syntax-error path (unelevated: no volume involved) ────────

#[test]
fn query_syntax_error_reports_cause_chain() {
    let h = create_engine();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = 0;

    // Parse-stage error: unclosed quote.
    let q = CString::new("\"abc").unwrap();
    assert_eq!(
        unsafe { fmf_query(h, q.as_ptr(), &opts, &mut rh, &mut count, ptr::null_mut()) },
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
        unsafe { fmf_query(h, q.as_ptr(), &opts, &mut rh, &mut count, ptr::null_mut()) },
        FMF_E_QUERY_SYNTAX
    );
    assert!(read_last_error().contains("invalid size filter"));

    // Compile-stage error (bad regex) maps to the same code.
    let q = CString::new("regex:[").unwrap();
    assert_eq!(
        unsafe { fmf_query(h, q.as_ptr(), &opts, &mut rh, &mut count, ptr::null_mut()) },
        FMF_E_QUERY_SYNTAX
    );
    let msg = read_last_error();
    assert!(msg.contains("query compile"), "missing stage: {msg}");
    assert!(msg.contains("caused by"), "missing cause chain: {msg}");

    destroy(h);
}

#[test]
fn valid_query_on_volumeless_engine_succeeds_empty() {
    // Contract: queries succeed against "Ready volumes only" — zero Ready
    // volumes is an empty result, not an error.
    let h = create_engine();
    let q = CString::new("foo").unwrap();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = u64::MAX;
    assert_eq!(
        unsafe { fmf_query(h, q.as_ptr(), &opts, &mut rh, &mut count, ptr::null_mut()) },
        FMF_OK
    );
    assert_eq!(count, 0);
    assert!(!rh.is_null(), "an empty result still yields a handle");

    let mut page: *mut FmfPage = ptr::null_mut();
    // result_page null matrix needs a live handle, so it lives here.
    assert_eq!(
        unsafe { fmf_result_page(ptr::null_mut(), 0, 1, &mut page) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(
        unsafe { fmf_result_page(rh, 0, 1, ptr::null_mut()) },
        FMF_E_INVALID_ARG
    );
    assert_eq!(unsafe { fmf_result_page(rh, 0, 16, &mut page) }, FMF_OK);
    assert_eq!(unsafe { (*page).row_count }, 0);
    assert_eq!(unsafe { fmf_page_free(page) }, FMF_OK);
    assert_eq!(unsafe { fmf_result_free(rh) }, FMF_OK);
    destroy(h);
}

// ── 3c. Page/blob packing roundtrip ─────────────────────────────────────

#[test]
fn page_packs_rows_and_string_blob_per_contract() {
    let h = ready_engine();
    let q = CString::new("alpha").unwrap();
    let opts = default_opts();
    let mut rh: *mut c_void = ptr::null_mut();
    let mut count: u64 = 0;
    let mut trace: *mut FmfBlob = ptr::null_mut();
    assert_eq!(
        unsafe { fmf_query(h, q.as_ptr(), &opts, &mut rh, &mut count, &mut trace) },
        FMF_OK
    );
    assert_eq!(count, 1);

    // out_trace (nullable, requested here): QueryTrace as UTF-8 JSON.
    let tjson = json_from_blob(trace);
    assert_eq!(tjson["query"], "alpha");
    assert_eq!(unsafe { fmf_blob_free(trace) }, FMF_OK);

    // One contiguous block: row header array + string blob, offsets into it.
    let mut page: *mut FmfPage = ptr::null_mut();
    assert_eq!(unsafe { fmf_result_page(rh, 0, 16, &mut page) }, FMF_OK);
    let p = unsafe { &*page };
    assert_eq!(p.row_count, 1);
    assert!(!p.rows.is_null());
    assert!(!p.blob.is_null());
    assert_eq!(p.blob_len as usize, "alpha.txt".len() + "C:\\".len());

    let row: &FmfRow = unsafe { &*p.rows };
    assert_eq!(row.entry_ref >> 32, 0, "volume ordinal in the high half");
    assert_eq!(row.frn, (1 << 48) | 100);
    assert_eq!(row.size, 1234);
    assert_eq!(row.mtime, 777);
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
    assert_eq!(unsafe { fmf_page_free(page) }, FMF_OK);

    // Out-of-range offsets page as empty, not as an error.
    let mut tail: *mut FmfPage = ptr::null_mut();
    assert_eq!(unsafe { fmf_result_page(rh, 999, 16, &mut tail) }, FMF_OK);
    assert_eq!(unsafe { (*tail).row_count }, 0);
    assert_eq!(unsafe { fmf_page_free(tail) }, FMF_OK);

    assert_eq!(unsafe { fmf_result_free(rh) }, FMF_OK);
    destroy(h);
}
