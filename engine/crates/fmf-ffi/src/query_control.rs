//! Monotonic opaque query-control lifecycles for the in-process ABI.
//!
//! Controls are registered before managed code schedules `fmf_query`, so a
//! cancellation that arrives before native execution cannot be lost.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::LazyLock;

use fmf_core::engine::QueryCancellation;
use parking_lot::Mutex;

use crate::error::{guard, guard_cleanup, set_error};
use crate::handle::engine;
use crate::opaque;
use crate::{FMF_E_INVALID_ARG, FMF_OK};

struct QueryControl {
    engine_id: usize,
    cancellation: QueryCancellation,
}

static CONTROLS: LazyLock<Mutex<HashMap<u64, QueryControl>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Create a query control owned by `h`. The returned nonzero ID is passed to
/// `fmf_query`, then freed after callback deregistration and query completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_query_control_create(h: *mut c_void, out_control_id: *mut u64) -> i32 {
    guard(|| {
        if out_control_id.is_null() {
            set_error("fmf_query_control_create requires a non-null out pointer");
            return FMF_E_INVALID_ARG;
        }
        unsafe { *out_control_id = 0 };
        let handle = match engine(h) {
            Ok(handle) => handle,
            Err(code) => return code,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(code) => return code,
        };
        let id = match opaque::next_id("query control") {
            Ok(id) => id as u64,
            Err(code) => return code,
        };
        CONTROLS.lock().insert(
            id,
            QueryControl {
                engine_id: handle.id,
                cancellation: QueryCancellation::new(),
            },
        );
        unsafe { *out_control_id = id };
        FMF_OK
    })
}

/// Idempotently request cancellation of a live query control.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_query_control_cancel(control_id: u64) -> i32 {
    guard(|| {
        let controls = CONTROLS.lock();
        let Some(control) = controls.get(&control_id) else {
            set_error(format!(
                "unknown, forged, or freed query control id: {control_id}"
            ));
            return FMF_E_INVALID_ARG;
        };
        control.cancellation.cancel();
        FMF_OK
    })
}

/// Free a completed query control. Unknown/double/stale IDs fail closed.
/// A successful cleanup preserves the preceding `fmf_query` diagnostic on
/// this caller thread until `fmf_last_error` can retrieve it.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_query_control_free(control_id: u64) -> i32 {
    guard_cleanup(|| {
        if control_id == 0 || CONTROLS.lock().remove(&control_id).is_none() {
            set_error(format!(
                "unknown, forged, or already freed query control id: {control_id}"
            ));
            FMF_E_INVALID_ARG
        } else {
            FMF_OK
        }
    })
}

pub(crate) fn cancellation(control_id: u64, engine_id: usize) -> Result<QueryCancellation, i32> {
    let controls = CONTROLS.lock();
    let Some(control) = controls.get(&control_id) else {
        set_error(format!(
            "unknown, forged, or freed query control id: {control_id}"
        ));
        return Err(FMF_E_INVALID_ARG);
    };
    if control.engine_id != engine_id {
        set_error("query control belongs to a different engine");
        return Err(FMF_E_INVALID_ARG);
    }
    Ok(control.cancellation.clone())
}

pub(crate) fn cancel_engine(engine_id: usize) {
    let mut controls = CONTROLS.lock();
    controls.retain(|_, control| {
        if control.engine_id == engine_id {
            control.cancellation.cancel();
            false
        } else {
            true
        }
    });
}
