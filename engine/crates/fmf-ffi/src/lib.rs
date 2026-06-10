//! fmf-ffi — C ABI surface over fmf-core (canonical contract:
//! docs/ARCHITECTURE.md). Conversion, handle management and panic catching
//! only; every function maps 1:1 onto a future named-pipe message.

// Safety contracts for every entry point live in docs/ARCHITECTURE.md (the
// canonical FFI contract) rather than per-function doc comments.
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use fmf_core::engine::{Engine, EngineConfig, EngineError, EngineEvent, ResultSet, VolumePhase};
use fmf_core::index::SortKey;
use fmf_core::query::{CaseMode, QueryOptions};

pub const FMF_OK: i32 = 0;
pub const FMF_E_INVALID_ARG: i32 = 1;
pub const FMF_E_STALE: i32 = 2;
pub const FMF_E_NOT_ADMIN: i32 = 3;
pub const FMF_E_VOLUME: i32 = 4;
pub const FMF_E_QUERY_SYNTAX: i32 = 5;
pub const FMF_E_IO: i32 = 6;
pub const FMF_E_PANIC: i32 = 99;

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

fn guard<F: FnOnce() -> i32>(f: F) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => {
            set_error("panic inside fmf_engine");
            FMF_E_PANIC
        }
    }
}

unsafe fn utf8_arg<'a>(p: *const c_char) -> Result<&'a str, i32> {
    if p.is_null() {
        set_error("null string argument");
        return Err(FMF_E_INVALID_ARG);
    }
    unsafe { CStr::from_ptr(p) }.to_str().map_err(|_| {
        set_error("argument is not valid UTF-8");
        FMF_E_INVALID_ARG
    })
}

// ── Handles ─────────────────────────────────────────────────────────────

struct EngineHandle {
    engine: Arc<Engine>,
    // Keeps the registered callback (and its user pointer) alive.
    _sink_keepalive: parking_lot::Mutex<Option<Arc<CallbackSink>>>,
}

#[unsafe(no_mangle)]
pub extern "C" fn fmf_abi_version() -> u32 {
    1
}

// ── Lifecycle ───────────────────────────────────────────────────────────

/// config_json: {"index_dir": "C:\\ProgramData\\find-my-files\\index"}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_engine_create(
    config_json: *const c_char,
    out: *mut *mut c_void,
) -> i32 {
    guard(|| {
        if out.is_null() {
            set_error("out handle is null");
            return FMF_E_INVALID_ARG;
        }
        let json = match unsafe { utf8_arg(config_json) } {
            Ok(s) => s,
            Err(c) => return c,
        };
        let parsed: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                set_error(format!("config json: {e}"));
                return FMF_E_INVALID_ARG;
            }
        };
        let Some(index_dir) = parsed.get("index_dir").and_then(|v| v.as_str()) else {
            set_error("config json: missing required key index_dir");
            return FMF_E_INVALID_ARG;
        };
        let engine = Engine::new(EngineConfig {
            index_dir: index_dir.into(),
        });
        let handle = Box::new(EngineHandle {
            engine,
            _sink_keepalive: parking_lot::Mutex::new(None),
        });
        unsafe { *out = Box::into_raw(handle).cast() };
        FMF_OK
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_engine_destroy(h: *mut c_void) -> i32 {
    guard(|| {
        if h.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let handle = unsafe { Box::from_raw(h.cast::<EngineHandle>()) };
        handle.engine.set_event_sink(None);
        handle.engine.shutdown();
        FMF_OK
    })
}

unsafe fn engine<'a>(h: *mut c_void) -> Result<&'a EngineHandle, i32> {
    if h.is_null() {
        set_error("null engine handle");
        return Err(FMF_E_INVALID_ARG);
    }
    Ok(unsafe { &*h.cast::<EngineHandle>() })
}

// ── Events ──────────────────────────────────────────────────────────────

pub const FMF_EVENT_PROGRESS: u32 = 1;
pub const FMF_EVENT_VOLUME_READY: u32 = 2;
pub const FMF_EVENT_INDEX_CHANGED: u32 = 3;
pub const FMF_EVENT_RESCAN_STARTED: u32 = 4;
pub const FMF_EVENT_VOLUME_FAILED: u32 = 5;

/// POD event payload. `volume` is NUL-terminated UTF-8 ("C:").
#[repr(C)]
pub struct FmfEvent {
    pub kind: u32,
    pub _pad: u32,
    pub entries: u64,
    pub volume: [u8; 16],
}

pub type FmfEventCb = Option<unsafe extern "C" fn(ev: *const FmfEvent, user: *mut c_void)>;

struct CallbackSink {
    cb: unsafe extern "C" fn(*const FmfEvent, *mut c_void),
    user: *mut c_void,
}
// Contract: the callback must be callable from any thread; the user pointer
// is treated as an opaque token.
unsafe impl Send for CallbackSink {}
unsafe impl Sync for CallbackSink {}

fn volume_bytes(label: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let bytes = label.as_bytes();
    let n = bytes.len().min(15);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_set_event_callback(
    h: *mut c_void,
    cb: FmfEventCb,
    user: *mut c_void,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        match cb {
            None => {
                handle.engine.set_event_sink(None);
                *handle._sink_keepalive.lock() = None;
            }
            Some(f) => {
                let sink = Arc::new(CallbackSink { cb: f, user });
                let keep = sink.clone();
                handle
                    .engine
                    .set_event_sink(Some(Arc::new(move |ev: &EngineEvent| {
                        let (kind, volume, entries) = match ev {
                            EngineEvent::Progress { volume, entries } => {
                                (FMF_EVENT_PROGRESS, volume, *entries)
                            }
                            EngineEvent::VolumeReady { volume, entries } => {
                                (FMF_EVENT_VOLUME_READY, volume, *entries)
                            }
                            EngineEvent::IndexChanged { volume } => {
                                (FMF_EVENT_INDEX_CHANGED, volume, 0)
                            }
                            EngineEvent::RescanStarted { volume } => {
                                (FMF_EVENT_RESCAN_STARTED, volume, 0)
                            }
                            EngineEvent::VolumeFailed { volume, .. } => {
                                (FMF_EVENT_VOLUME_FAILED, volume, 0)
                            }
                        };
                        let payload = FmfEvent {
                            kind,
                            _pad: 0,
                            entries,
                            volume: volume_bytes(volume),
                        };
                        unsafe { (sink.cb)(&payload, sink.user) };
                    })));
                *handle._sink_keepalive.lock() = Some(keep);
            }
        }
        FMF_OK
    })
}

// ── Volumes & indexing ──────────────────────────────────────────────────

#[repr(C)]
pub struct FmfVolumeStatus {
    pub label: [u8; 16],
    pub state: u32, // 0=Scanning 1=Ready 2=Rescanning 3=Failed
    pub _pad: u32,
    pub entries: u64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_list_volumes(
    _h: *mut c_void,
    buf: *mut FmfVolumeStatus,
    cap: u32,
    count: *mut u32,
) -> i32 {
    guard(|| {
        if count.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let vols = Engine::list_ntfs_volumes();
        unsafe { *count = vols.len() as u32 };
        if !buf.is_null() {
            for (i, v) in vols.iter().take(cap as usize).enumerate() {
                unsafe {
                    *buf.add(i) = FmfVolumeStatus {
                        label: volume_bytes(v),
                        state: 0,
                        _pad: 0,
                        entries: 0,
                    };
                }
            }
        }
        FMF_OK
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_start(
    h: *mut c_void,
    volumes: *const *const c_char,
    n: u32,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if volumes.is_null() && n > 0 {
            return FMF_E_INVALID_ARG;
        }
        let mut labels = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            match unsafe { utf8_arg(*volumes.add(i)) } {
                Ok(s) => labels.push(s.to_string()),
                Err(c) => return c,
            }
        }
        handle.engine.index_start(&labels);
        FMF_OK
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_status(
    h: *mut c_void,
    buf: *mut FmfVolumeStatus,
    cap: u32,
    count: *mut u32,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if count.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let status = handle.engine.status();
        unsafe { *count = status.len() as u32 };
        if !buf.is_null() {
            for (i, (label, phase, entries)) in status.iter().take(cap as usize).enumerate() {
                let state = match phase {
                    VolumePhase::Scanning => 0,
                    VolumePhase::Ready => 1,
                    VolumePhase::Rescanning => 2,
                    VolumePhase::Failed => 3,
                };
                unsafe {
                    *buf.add(i) = FmfVolumeStatus {
                        label: volume_bytes(label),
                        state,
                        _pad: 0,
                        entries: *entries,
                    };
                }
            }
        }
        FMF_OK
    })
}

// ── Query & paging ──────────────────────────────────────────────────────

#[repr(C)]
pub struct FmfQueryOptions {
    pub sort: u32, // 0=Name 1=Size 2=Mtime
    pub desc: u32,
    pub case_mode: u32, // 0=Smart 1=Insensitive 2=Sensitive
    /// Nonzero shows hidden/system entries (default-excluded otherwise).
    pub include_hidden_system: u32,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_query(
    h: *mut c_void,
    query_utf8: *const c_char,
    options: *const FmfQueryOptions,
    out_handle: *mut *mut c_void,
    out_count: *mut u64,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if out_handle.is_null() || out_count.is_null() || options.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let text = match unsafe { utf8_arg(query_utf8) } {
            Ok(s) => s,
            Err(c) => return c,
        };
        let o = unsafe { &*options };
        let opt = QueryOptions {
            sort: match o.sort {
                1 => SortKey::Size,
                2 => SortKey::Mtime,
                _ => SortKey::Name,
            },
            desc: o.desc != 0,
            case: match o.case_mode {
                1 => CaseMode::Insensitive,
                2 => CaseMode::Sensitive,
                _ => CaseMode::Smart,
            },
            include_hidden_system: o.include_hidden_system != 0,
        };
        match handle.engine.query(text, &opt) {
            Ok((rs, _trace)) => {
                unsafe {
                    *out_count = rs.len() as u64;
                    *out_handle = Box::into_raw(Box::new(rs)).cast();
                }
                FMF_OK
            }
            Err(e @ (EngineError::Parse(_) | EngineError::Compile(_))) => {
                set_error(e.to_string());
                FMF_E_QUERY_SYNTAX
            }
            Err(e) => {
                set_error(e.to_string());
                FMF_E_STALE
            }
        }
    })
}

/// 48-byte row, no internal padding. Mirrored by C# LayoutKind.Sequential.
#[repr(C)]
pub struct FmfRow {
    pub entry_ref: u64,
    pub frn: u64,
    pub size: u64,
    pub mtime: i64,
    pub name_off: u32,
    pub parent_path_off: u32,
    pub flags: u32,
    pub name_len: u16,
    pub parent_path_len: u16,
}

#[repr(C)]
pub struct FmfPage {
    pub row_count: u32,
    pub _pad: u32,
    pub rows: *const FmfRow,
    pub blob: *const u8,
    pub blob_len: u32,
    pub _pad2: u32,
}

#[repr(C)]
struct PageOwned {
    page: FmfPage, // must stay the first field: its address is the handle
    rows: Vec<FmfRow>,
    blob: Vec<u8>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_result_page(
    r: *mut c_void,
    offset: u64,
    count: u32,
    out: *mut *mut FmfPage,
) -> i32 {
    guard(|| {
        if r.is_null() || out.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let rs = unsafe { &*r.cast::<ResultSet>() };
        let rows_data = match rs.page(offset as usize, count as usize) {
            Ok(rows) => rows,
            Err(EngineError::Stale) => {
                set_error("structural generation moved; re-run the query");
                return FMF_E_STALE;
            }
            Err(e) => {
                set_error(e.to_string());
                return FMF_E_IO;
            }
        };

        let mut blob = Vec::new();
        let mut rows = Vec::with_capacity(rows_data.len());
        for row in &rows_data {
            let name_off = blob.len() as u32;
            blob.extend_from_slice(&row.name);
            let parent_off = blob.len() as u32;
            blob.extend_from_slice(&row.parent_path);
            rows.push(FmfRow {
                entry_ref: row.entry_ref,
                frn: row.frn,
                size: row.size,
                mtime: row.mtime,
                name_off,
                parent_path_off: parent_off,
                flags: row.flags,
                name_len: row.name.len() as u16,
                parent_path_len: row.parent_path.len() as u16,
            });
        }
        let mut owned = Box::new(PageOwned {
            page: FmfPage {
                row_count: rows.len() as u32,
                _pad: 0,
                rows: std::ptr::null(),
                blob: std::ptr::null(),
                blob_len: blob.len() as u32,
                _pad2: 0,
            },
            rows,
            blob,
        });
        owned.page.rows = owned.rows.as_ptr();
        owned.page.blob = owned.blob.as_ptr();
        unsafe { *out = Box::into_raw(owned).cast() };
        FMF_OK
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_page_free(p: *mut FmfPage) -> i32 {
    guard(|| {
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p.cast::<PageOwned>()) });
        }
        FMF_OK
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_result_free(r: *mut c_void) -> i32 {
    guard(|| {
        if !r.is_null() {
            drop(unsafe { Box::from_raw(r.cast::<ResultSet>()) });
        }
        FMF_OK
    })
}

// ── Diagnostics ─────────────────────────────────────────────────────────

/// Copies the thread-local detail message. `len` is in/out (capacity →
/// written bytes, excluding the NUL this function appends when room allows).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_last_error(buf: *mut u8, len: *mut u32) -> i32 {
    if len.is_null() {
        return FMF_E_INVALID_ARG;
    }
    LAST_ERROR.with(|e| {
        let msg = e.borrow();
        let bytes = msg.as_bytes();
        let cap = unsafe { *len } as usize;
        let n = bytes.len().min(cap.saturating_sub(1));
        if !buf.is_null() && cap > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
                *buf.add(n) = 0;
            }
        }
        unsafe { *len = n as u32 };
    });
    FMF_OK
}
