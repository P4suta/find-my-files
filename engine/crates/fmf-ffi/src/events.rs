use std::ffi::c_void;
use std::sync::Arc;

use fmf_core::engine::EngineEvent;

use crate::FMF_OK;
use crate::error::guard;
use crate::handle::engine;

// ── Events ──────────────────────────────────────────────────────────────

// Event kinds and the POD radiate from the contract (ADR-0018).
pub use fmf_contract::events::{
    FMF_EVENT_ENGINE_ERROR, FMF_EVENT_INDEX_CHANGED, FMF_EVENT_PROGRESS, FMF_EVENT_RESCAN_STARTED,
    FMF_EVENT_VOLUME_FAILED, FMF_EVENT_VOLUME_READY,
};
pub use fmf_contract::pod::FmfEvent;

pub type FmfEventCb = Option<unsafe extern "C" fn(ev: *const FmfEvent, user: *mut c_void)>;

pub(crate) struct CallbackSink {
    cb: unsafe extern "C" fn(*const FmfEvent, *mut c_void),
    user: *mut c_void,
}
// Contract: the callback must be callable from any thread; the user pointer
// is treated as an opaque token.
unsafe impl Send for CallbackSink {}
unsafe impl Sync for CallbackSink {}

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
                        // EngineEvent::to_wire is the single kind mapping.
                        let payload = ev.to_wire();
                        unsafe { (sink.cb)(&raw const payload, sink.user) };
                    })));
                *handle._sink_keepalive.lock() = Some(keep);
            }
        }
        FMF_OK
    })
}
