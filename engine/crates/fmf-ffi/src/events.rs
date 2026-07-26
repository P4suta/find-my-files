use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Arc;

use fmf_core::engine::EngineEvent;
use parking_lot::{Condvar, Mutex};

use crate::error::{guard, set_error};
use crate::handle::{EngineHandle, engine};
use crate::{FMF_E_INVALID_ARG, FMF_OK};

// ── Events ──────────────────────────────────────────────────────────────

// Event kinds and the POD radiate from the contract (ADR-0018).
pub use fmf_contract::events::{
    FMF_EVENT_ENGINE_ERROR, FMF_EVENT_INDEX_CHANGED, FMF_EVENT_PROGRESS, FMF_EVENT_RESCAN_STARTED,
    FMF_EVENT_VOLUME_FAILED, FMF_EVENT_VOLUME_READY,
};
pub use fmf_contract::pod::FmfEvent;

/// Callback the host registers to receive engine events; invoked with a borrowed
/// `FmfEvent` and the opaque `user` token. `None` clears the registration.
pub type FmfEventCb = Option<unsafe extern "C" fn(ev: *const FmfEvent, user: *mut c_void)>;

pub(crate) struct CallbackSink {
    cb: unsafe extern "C" fn(*const FmfEvent, *mut c_void),
    user: *mut c_void,
    state: Mutex<CallbackState>,
    idle: Condvar,
}

struct CallbackState {
    accepting: bool,
    in_flight: usize,
}

// Contract: the callback must be callable from any thread; the user pointer
// is treated as an opaque token.
unsafe impl Send for CallbackSink {}
unsafe impl Sync for CallbackSink {}

thread_local! {
    static CALLBACK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct CallbackScope;

impl CallbackScope {
    fn enter() -> Self {
        CALLBACK_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for CallbackScope {
    fn drop(&mut self) {
        CALLBACK_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

impl CallbackSink {
    pub(crate) fn new(
        cb: unsafe extern "C" fn(*const FmfEvent, *mut c_void),
        user: *mut c_void,
    ) -> Self {
        Self {
            cb,
            user,
            state: Mutex::new(CallbackState {
                accepting: true,
                in_flight: 0,
            }),
            idle: Condvar::new(),
        }
    }

    pub(crate) fn invoke(&self, payload: &FmfEvent) {
        {
            let mut state = self.state.lock();
            if !state.accepting {
                return;
            }
            state.in_flight += 1;
        }

        let _scope = CallbackScope::enter();
        unsafe { (self.cb)(payload, self.user) };

        let mut state = self.state.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.idle.notify_all();
        }
    }

    pub(crate) fn deactivate_and_wait(&self) {
        let mut state = self.state.lock();
        state.accepting = false;
        while state.in_flight != 0 {
            self.idle.wait(&mut state);
        }
    }
}

pub(crate) fn callback_active_on_current_thread() -> bool {
    CALLBACK_DEPTH.with(|depth| depth.get() != 0)
}

/// Detach the engine closure, then wait for callbacks that cloned it before
/// detachment. On return the foreign `user` token is no longer observable.
pub(crate) fn clear_event_callback(handle: &EngineHandle) {
    let mut registered = handle._sink_keepalive.lock();
    handle.engine.set_event_sink(None);
    if let Some(sink) = registered.take() {
        sink.deactivate_and_wait();
    }
}

/// Registers (or clears, when `cb` is `None`) the event callback for the engine
/// handle `h`; the callback may fire from any thread.
///
/// Successful replacement or
/// clearing is a quiescence barrier: the old callback cannot run after this
/// function returns. Lifecycle mutation from inside a callback is rejected to
/// avoid self-wait deadlock. Returns `FMF_OK` or an error code. Safety: see
/// docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_set_event_callback(
    h: *mut c_void,
    cb: FmfEventCb,
    user: *mut c_void,
) -> i32 {
    guard(|| {
        if callback_active_on_current_thread() {
            set_error("fmf_set_event_callback cannot be called from an engine event callback");
            return FMF_E_INVALID_ARG;
        }
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };

        let mut registered = handle._sink_keepalive.lock();
        handle.engine.set_event_sink(None);
        if let Some(old) = registered.take() {
            old.deactivate_and_wait();
        }

        if let Some(f) = cb {
            let sink = Arc::new(CallbackSink::new(f, user));
            let dispatch = sink.clone();
            handle
                .engine
                .set_event_sink(Some(Arc::new(move |ev: &EngineEvent| {
                    // EngineEvent::to_wire is the single kind mapping.
                    let payload = ev.to_wire();
                    dispatch.invoke(&payload);
                })));
            *registered = Some(sink);
        }
        FMF_OK
    })
}
