use std::ffi::{c_char, c_void};

use fmf_core::engine::{EngineError, ResultSet};
use fmf_core::query::QueryOptions;

use crate::blob::{FmfBlob, blob_from_json};
use crate::error::{error_chain, guard, set_error, utf8_arg};
use crate::handle::engine;
use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_QUERY_SYNTAX, FMF_E_STALE, FMF_OK};

// ── Query & paging ──────────────────────────────────────────────────────

// The query/page PODs radiate from the contract (ADR-0018).
pub use fmf_contract::pod::{FmfPage, FmfQueryOptions, FmfRow};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_query(
    h: *mut c_void,
    query_utf8: *const c_char,
    options: *const FmfQueryOptions,
    out_handle: *mut *mut c_void,
    out_count: *mut u64,
    out_trace: *mut *mut FmfBlob, // nullable: stage breakdown as JSON
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
        let opt: QueryOptions = unsafe { *options }.into();
        match handle.engine.query(text, &opt) {
            Ok((rs, trace)) => {
                unsafe {
                    *out_count = rs.len() as u64;
                    *out_handle = Box::into_raw(Box::new(rs)).cast();
                    if !out_trace.is_null() {
                        *out_trace = match serde_json::to_string(&trace) {
                            Ok(json) => blob_from_json(json),
                            Err(e) => {
                                // 黙らない: counted + warned; the query itself
                                // succeeded, the trace is explicitly absent.
                                fmf_core::degrade!(
                                    handle.engine.metrics().counters.trace_serialize_failures,
                                    error = %e,
                                    "query trace serialization failed — returning null trace"
                                );
                                std::ptr::null_mut()
                            }
                        };
                    }
                }
                FMF_OK
            }
            Err(e @ (EngineError::Parse(_) | EngineError::Compile(_))) => {
                set_error(error_chain(&e));
                FMF_E_QUERY_SYNTAX
            }
            Err(e) => {
                set_error(error_chain(&e));
                FMF_E_STALE
            }
        }
    })
}

#[repr(C)]
struct PageOwned {
    page: FmfPage, // must stay the first field: its address is the handle
    rows: Vec<FmfRow>,
    blob: Vec<u8>,
}

// The handle handed to C# is a `*mut FmfPage` that actually points at a boxed
// `PageOwned` (see `fmf_result_page`/`fmf_page_free`); the cast is sound only
// while `page` sits at offset 0. Pin it at compile time so a future field
// reorder breaks the build instead of dangling a caller's pointer.
const _: () = assert!(std::mem::offset_of!(PageOwned, page) == 0);

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
        // The row+blob packing is fmf-core's single implementation
        // (ResultSet::fill_page) — this layer only wraps it in FmfPage.
        let (rows, blob) = match rs.fill_page(offset as usize, count as usize) {
            Ok(page) => page,
            Err(EngineError::Stale) => {
                set_error("structural generation moved; re-run the query");
                return FMF_E_STALE;
            }
            Err(e) => {
                set_error(e.to_string());
                return FMF_E_IO;
            }
        };
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
