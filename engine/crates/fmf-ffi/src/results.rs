use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock};

use fmf_core::engine::{EngineError, ResultSet};
use fmf_core::query::QueryOptions;
use parking_lot::Mutex;

use crate::allocation::next_owner_id;
use crate::blob::{FmfBlob, blob_from_json};
use crate::error::{checked_abi_len, error_chain, guard, set_error, utf8_arg};
use crate::handle::engine;
use crate::opaque;
use crate::query_control;
use crate::{
    FMF_E_CANCELLED, FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_QUERY_SYNTAX, FMF_E_STALE, FMF_OK,
};

// ── Query & paging ──────────────────────────────────────────────────────

// The query/page PODs radiate from the contract (ADR-0018).
pub use fmf_contract::pod::{FmfPage, FmfQueryOptions, FmfRow};

/// Per-query correlation counter for the in-process FFI path (see `fmf_query`).
static FFI_QID: AtomicU64 = AtomicU64::new(1);
struct ResultEntry {
    engine_id: usize,
    set: Arc<ResultSet>,
}

static RESULTS: LazyLock<Mutex<HashMap<usize, ResultEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn insert_result(engine_id: usize, result: ResultSet) -> Result<*mut c_void, i32> {
    let id = opaque::next_id("result")?;
    RESULTS.lock().insert(
        id,
        ResultEntry {
            engine_id,
            set: Arc::new(result),
        },
    );
    Ok(opaque::to_ptr(id))
}

pub(crate) fn result(r: *mut c_void) -> Result<Arc<ResultSet>, i32> {
    if r.is_null() {
        set_error("null result handle");
        return Err(FMF_E_INVALID_ARG);
    }
    let id = opaque::id_of(r);
    RESULTS
        .lock()
        .get(&id)
        .map(|entry| Arc::clone(&entry.set))
        .ok_or_else(|| {
            set_error(format!("unknown or freed result handle: {id}"));
            FMF_E_INVALID_ARG
        })
}

pub(crate) fn purge_engine(engine_id: usize) {
    RESULTS
        .lock()
        .retain(|_, entry| entry.engine_id != engine_id);
}

/// Runs a query against the engine, returning an opaque result-set handle plus
/// the total match count and an optional JSON query trace.
///
/// Writes the result-set handle to `out_handle`, the match count to `out_count`,
/// and (when `out_trace` is non-null) a `FmfBlob` holding the stage-breakdown
/// trace as JSON. Returns `FMF_OK` on success or an `FMF_E_*` code.
/// Safety: see docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_query(
    h: *mut c_void,
    query_utf8: *const c_char,
    options: *const FmfQueryOptions,
    query_control_id: u64,
    out_handle: *mut *mut c_void,
    out_count: *mut u64,
    out_trace: *mut *mut FmfBlob, // nullable: stage breakdown as JSON
) -> i32 {
    guard(|| {
        if out_handle.is_null() || out_count.is_null() || options.is_null() {
            set_error("fmf_query requires non-null options, out_handle, and out_count");
            return FMF_E_INVALID_ARG;
        }
        unsafe {
            *out_handle = std::ptr::null_mut();
            *out_count = 0;
            if !out_trace.is_null() {
                *out_trace = std::ptr::null_mut();
            }
        }
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };
        let cancellation = match query_control::cancellation(query_control_id, handle.id) {
            Ok(cancellation) => cancellation,
            Err(code) => return code,
        };
        let text = match unsafe { utf8_arg(query_utf8) } {
            Ok(s) => s,
            Err(c) => return c,
        };
        let wire_opt = unsafe { *options };
        let opt = match QueryOptions::try_from(wire_opt) {
            Ok(options) => options,
            Err(e) => {
                set_error(e.to_string());
                return FMF_E_INVALID_ARG;
            }
        };
        // Per-query correlation: an in-process counter groups this request's
        // engine-side log lines under `qid`. The cross-log join key for the
        // (single-process) FFI path is `rid` — the result handle's address,
        // which the UI also holds — since there is no wire request id here
        // (ADR-0037). The contract is unchanged.
        let _qid = tracing::info_span!("req", qid = FFI_QID.fetch_add(1, AtomicOrdering::Relaxed))
            .entered();
        let basis_id = usize::try_from(wire_opt.presentation_basis).ok();
        let basis = basis_id.and_then(|id| {
            RESULTS
                .lock()
                .get(&id)
                .and_then(|entry| (entry.engine_id == handle.id).then(|| Arc::clone(&entry.set)))
        });
        match handle
            .engine
            .query_cancellable(text, &opt, &cancellation, basis.as_deref())
        {
            Ok((rs, mut trace)) => {
                if cancellation.is_cancelled() {
                    set_error("query cancelled");
                    return FMF_E_CANCELLED;
                }
                if let (Some(basis_id), Some(basis)) = (basis_id, basis.as_ref()) {
                    trace.unchanged &= RESULTS.lock().get(&basis_id).is_some_and(|entry| {
                        entry.engine_id == handle.id && Arc::ptr_eq(&entry.set, basis)
                    });
                }
                let count = rs.len() as u64;
                let raw = match insert_result(handle.id, rs) {
                    Ok(raw) => raw,
                    Err(code) => return code,
                };
                let trace_blob = if out_trace.is_null() {
                    std::ptr::null_mut()
                } else {
                    match serde_json::to_string(&trace) {
                        Ok(json) => match blob_from_json(json) {
                            Ok(blob) => blob,
                            Err(code) => {
                                RESULTS.lock().remove(&opaque::id_of(raw));
                                return code;
                            }
                        },
                        Err(e) => {
                            // don't go silent: counted + warned; the query itself
                            // succeeded, the trace is explicitly absent.
                            fmf_core::degrade!(
                                handle.engine.metrics().counters.trace_serialize_failures,
                                error = %e,
                                "query trace serialization failed — returning null trace"
                            );
                            std::ptr::null_mut()
                        }
                    }
                };
                fmf_core::diag::log_query_served(opaque::id_of(raw) as u64, &trace);
                unsafe {
                    *out_count = count;
                    *out_handle = raw;
                    if !out_trace.is_null() {
                        *out_trace = trace_blob;
                    }
                }
                FMF_OK
            }
            Err(e @ (EngineError::Parse(_) | EngineError::Compile(_))) => {
                set_error(error_chain(&e));
                FMF_E_QUERY_SYNTAX
            }
            Err(e @ EngineError::QueryTooLong { .. }) => {
                set_error(e.to_string());
                FMF_E_INVALID_ARG
            }
            Err(EngineError::Cancelled) => {
                set_error("query cancelled");
                FMF_E_CANCELLED
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
    page: FmfPage, // published descriptor; the Box keeps its address stable
    rows: Vec<FmfRow>,
    blob: Vec<u8>,
}

// SAFETY: `page.rows`/`page.blob` are null for empty buffers and otherwise
// point into the two owned Vec allocations. Moving the Box does not move those
// allocations, and the owner is immutable after publication. The registry
// mutex serializes ownership transfer/removal; callers must not read while
// concurrently freeing the page.
#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the raw descriptor pointers target this Box's immutable Vec allocations"
)]
unsafe impl Send for PageOwned {}

static PAGES: LazyLock<Mutex<HashMap<u64, Box<PageOwned>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn insert_page(mut owned: Box<PageOwned>) -> Result<*mut FmfPage, i32> {
    let owner_id = next_owner_id("result page")?;
    owned.page.owner_id = owner_id;
    owned.page.rows = if owned.rows.is_empty() {
        std::ptr::null()
    } else {
        owned.rows.as_ptr()
    };
    owned.page.blob = if owned.blob.is_empty() {
        std::ptr::null()
    } else {
        owned.blob.as_ptr()
    };
    let page_ptr = std::ptr::from_mut(&mut owned.page);
    let mut pages = PAGES.lock();
    match pages.entry(owner_id) {
        Entry::Vacant(entry) => {
            entry.insert(owned);
        }
        Entry::Occupied(_) => {
            set_error(format!(
                "duplicate result-page allocation owner id: {owner_id}"
            ));
            return Err(FMF_E_IO);
        }
    }
    Ok(page_ptr)
}

/// Materializes a window of rows from a result-set handle into a freshly
/// allocated `FmfPage`.
///
/// Fills `count` rows starting at `offset` and writes the owning page pointer to
/// `out`; free it with `fmf_page_free`. Returns `FMF_OK`, or `FMF_E_STALE` if the
/// structural generation moved (re-run the query), or another `FMF_E_*` code.
/// Safety: see docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_result_page(
    r: *mut c_void,
    offset: u64,
    count: u32,
    out: *mut *mut FmfPage,
) -> i32 {
    guard(|| {
        if out.is_null() {
            set_error("fmf_result_page requires a non-null out pointer");
            return FMF_E_INVALID_ARG;
        }
        unsafe { *out = std::ptr::null_mut() };
        let rs = match result(r) {
            Ok(result) => result,
            Err(code) => return code,
        };
        let Ok(offset) = usize::try_from(offset) else {
            set_error("fmf_result_page offset exceeds the supported address space");
            return FMF_E_INVALID_ARG;
        };
        // `count` is u32, which is losslessly representable by usize on every
        // supported Windows target.
        let count = count as usize;
        // The row+blob packing is fmf-core's single implementation
        // (ResultSet::fill_page) — this layer only wraps it in FmfPage.
        let (rows, blob) = match rs.fill_page(offset, count) {
            Ok(page) => page,
            Err(EngineError::Stale) => {
                set_error("structural generation moved; re-run the query");
                return FMF_E_STALE;
            }
            Err(e @ EngineError::PageTooLarge { .. }) => {
                set_error(e.to_string());
                return FMF_E_INVALID_ARG;
            }
            Err(e) => {
                set_error(e.to_string());
                return FMF_E_IO;
            }
        };
        let row_count = match checked_abi_len(rows.len(), "page row count") {
            Ok(len) => len,
            Err(code) => return code,
        };
        let blob_len = match checked_abi_len(blob.len(), "page string blob") {
            Ok(len) => len,
            Err(code) => return code,
        };
        let owned = Box::new(PageOwned {
            page: FmfPage {
                row_count,
                _pad: 0,
                rows: std::ptr::null(),
                blob: std::ptr::null(),
                blob_len,
                _pad2: 0,
                owner_id: 0,
            },
            rows,
            blob,
        });
        let page_ptr = match insert_page(owned) {
            Ok(page) => page,
            Err(code) => return code,
        };
        unsafe { *out = page_ptr };
        FMF_OK
    })
}

/// Frees the result page identified by the monotonic owner ID returned in its descriptor.
///
/// ID zero is a no-op; unknown, wrong-kind, stale, and already freed IDs are
/// rejected without touching foreign memory.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_page_free(owner_id: u64) -> i32 {
    guard(|| {
        if owner_id == 0 {
            return FMF_OK;
        }
        if PAGES.lock().remove(&owner_id).is_some() {
            FMF_OK
        } else {
            set_error(format!(
                "unknown, wrong-kind, or already freed result-page owner id: {owner_id}"
            ));
            FMF_E_INVALID_ARG
        }
    })
}

/// Frees a result-set handle previously returned by `fmf_query`. Null is a
/// no-op. Returns `FMF_OK`. Safety: see docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_result_free(r: *mut c_void) -> i32 {
    guard(|| {
        if r.is_null() {
            return FMF_OK;
        }
        let id = opaque::id_of(r);
        if RESULTS.lock().remove(&id).is_some() {
            FMF_OK
        } else {
            set_error(format!("unknown or already freed result handle: {id}"));
            FMF_E_INVALID_ARG
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{FmfPage, PageOwned, fmf_page_free, insert_page};
    use crate::blob::{blob_from_json, fmf_blob_free};
    use crate::{FMF_E_INVALID_ARG, FMF_OK};

    fn empty_page() -> *mut FmfPage {
        insert_page(Box::new(PageOwned {
            page: FmfPage {
                row_count: 0,
                _pad: 0,
                rows: std::ptr::null(),
                blob: std::ptr::null(),
                blob_len: 0,
                _pad2: 0,
                owner_id: 0,
            },
            rows: Vec::new(),
            blob: Vec::new(),
        }))
        .expect("test page allocation")
    }

    #[test]
    fn page_registry_rejects_double_free_forged_ids_and_aba() {
        let page = empty_page();
        let stale_id = unsafe { (*page).owner_id };
        assert_ne!(stale_id, 0);
        assert_eq!(fmf_page_free(stale_id), FMF_OK);
        assert_eq!(fmf_page_free(stale_id), FMF_E_INVALID_ARG);

        let replacement = empty_page();
        let replacement_id = unsafe { (*replacement).owner_id };
        assert_ne!(replacement_id, stale_id, "owner IDs are never reused");
        assert_eq!(
            fmf_page_free(stale_id),
            FMF_E_INVALID_ARG,
            "a stale owner ID cannot free a replacement allocation"
        );
        assert_eq!(fmf_page_free(replacement_id), FMF_OK);
        assert_eq!(fmf_page_free(u64::MAX), FMF_E_INVALID_ARG);
    }

    #[test]
    fn page_and_blob_allocations_cannot_be_cross_freed() {
        let page = empty_page();
        let page_id = unsafe { (*page).owner_id };
        assert_eq!(
            fmf_blob_free(page_id),
            FMF_E_INVALID_ARG,
            "a page owner ID is not a blob owner"
        );
        assert_eq!(fmf_page_free(page_id), FMF_OK);

        let blob = blob_from_json("{}".to_owned()).expect("test blob allocation");
        let blob_id = unsafe { (*blob).owner_id };
        assert_eq!(
            fmf_page_free(blob_id),
            FMF_E_INVALID_ARG,
            "a blob owner ID is not a page owner"
        );
        assert_eq!(fmf_blob_free(blob_id), FMF_OK);
    }
}
