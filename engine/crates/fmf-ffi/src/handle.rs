use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use fmf_core::engine::{Engine, EngineConfig, EngineCreateError};
use parking_lot::{Condvar, Mutex};
use serde::Deserialize;

use crate::error::{guard, set_error, utf8_arg};
use crate::events::{CallbackSink, callback_active_on_current_thread, clear_event_callback};
use crate::opaque;
use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_LOCKED, FMF_OK};

// ── Handles ─────────────────────────────────────────────────────────────

pub(crate) struct EngineHandle {
    pub(crate) id: usize,
    pub(crate) engine: Arc<Engine>,
    /// Admission is counted rather than protected by an `RwLock`: an event
    /// callback may make a (non-lifecycle) FFI call while destroy is waiting,
    /// and a writer-preferring lock would deadlock that re-entry.
    lifecycle: Mutex<Lifecycle>,
    lifecycle_idle: Condvar,
    // Serializes callback replacement and keeps the callback/user pair alive.
    pub(crate) _sink_keepalive: Mutex<Option<Arc<CallbackSink>>>,
}

struct Lifecycle {
    accepting: bool,
    in_flight: usize,
}

pub(crate) struct EngineCall {
    handle: Arc<EngineHandle>,
}

impl Drop for EngineCall {
    fn drop(&mut self) {
        let mut state = self.handle.lifecycle.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.handle.lifecycle_idle.notify_all();
        }
    }
}

impl EngineHandle {
    pub(crate) fn enter(self: &Arc<Self>) -> Result<EngineCall, i32> {
        let mut state = self.lifecycle.lock();
        if !state.accepting {
            set_error("engine handle is being destroyed");
            return Err(FMF_E_INVALID_ARG);
        }
        state.in_flight += 1;
        drop(state);
        Ok(EngineCall {
            handle: self.clone(),
        })
    }

    fn begin_destroy(&self) {
        let mut state = self.lifecycle.lock();
        state.accepting = false;
        while state.in_flight != 0 {
            self.lifecycle_idle.wait(&mut state);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_accepting_for_test(&self) -> bool {
        self.lifecycle.lock().accepting
    }
}

static ENGINES: LazyLock<Mutex<HashMap<usize, Arc<EngineHandle>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineCreateConfig {
    index_dir: PathBuf,
    #[serde(default)]
    log_dir: Option<PathBuf>,
    #[serde(default = "default_log_level")]
    log_level: String,
}

fn default_log_level() -> String {
    "info".to_owned()
}

/// Returns the engine ABI version the C# host must match before calling any other entry point.
#[unsafe(no_mangle)]
pub const extern "C" fn fmf_abi_version() -> u32 {
    fmf_contract::versions::ABI_VERSION
}

// ── Lifecycle ───────────────────────────────────────────────────────────

/// `config_json`: {"`index_dir"`: "C:\\`ProgramData`\\find-my-files\\index"}
///
/// # Safety
///
/// `config_json` must point to readable, NUL-terminated UTF-8 for this call.
/// `out` must be aligned and writable as one pointer, and must not overlap the
/// configuration string. On entry the function stores null in `out`; a
/// successful call replaces it with a registry-owned opaque handle.
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
        unsafe { *out = std::ptr::null_mut() };
        let json = match unsafe { utf8_arg(config_json) } {
            Ok(s) => s,
            Err(c) => return c,
        };
        let parsed: EngineCreateConfig = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                set_error(format!("config json: {e}"));
                return FMF_E_INVALID_ARG;
            }
        };

        // Diagnostics first: everything after this point is observable
        // (file log, diag ring, ENGINE_ERROR events). Resolution rule and
        // bootstrap live in fmf-core::diag — the single home (ADR-0018).
        let log_dir = fmf_core::diag::resolve_log_dir(parsed.log_dir, &parsed.index_dir);
        fmf_core::diag::init_diag(
            Some(&log_dir),
            &parsed.log_level,
            fmf_core::diag::DEFAULT_MAX_LOG_FILES,
        );

        let engine = match Engine::new(EngineConfig {
            index_dir: parsed.index_dir,
        }) {
            Ok(e) => e,
            Err(e @ EngineCreateError::Locked(_)) => {
                set_error(e.to_string());
                return FMF_E_LOCKED;
            }
            Err(e) => {
                set_error(e.to_string());
                return FMF_E_IO;
            }
        };
        let id = match opaque::next_id("engine") {
            Ok(id) => id,
            Err(code) => return code,
        };
        let handle = Arc::new(EngineHandle {
            id,
            engine,
            lifecycle: Mutex::new(Lifecycle {
                accepting: true,
                in_flight: 0,
            }),
            lifecycle_idle: Condvar::new(),
            _sink_keepalive: Mutex::new(None),
        });
        ENGINES.lock().insert(id, handle);
        unsafe { *out = opaque::to_ptr(id) };
        FMF_OK
    })
}

/// Snapshot-saves every Ready volume that is dirty (its content generation
/// advanced since the last save) and returns once they are written.
///
/// Deliberately has no pipe opcode: the service flushes on its own schedule
/// and at stop, so no client can turn this into a disk-write amplifier.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_flush(h: *mut c_void) -> i32 {
    guard(|| {
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };
        handle.engine.flush();
        FMF_OK
    })
}

/// Destroys an engine handle after draining its admitted work.
///
/// This detaches the event sink, waits for admitted calls and callbacks, shuts
/// the engine down, and invalidates the opaque handle. Concurrent calls already
/// admitted may finish; later and duplicate uses fail closed.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_engine_destroy(h: *mut c_void) -> i32 {
    guard(|| {
        if callback_active_on_current_thread() {
            set_error("fmf_engine_destroy cannot be called from an engine event callback");
            return FMF_E_INVALID_ARG;
        }
        let handle = match take_engine(h) {
            Ok(handle) => handle,
            Err(c) => return c,
        };
        crate::query_control::cancel_engine(handle.id);
        handle.begin_destroy();
        // A control creation admitted just before `begin_destroy` can insert
        // after the first sweep. The lifecycle barrier above waits for every
        // such call to retire; sweep once more so destruction leaves no
        // engine-owned control orphaned in the process-global registry.
        crate::query_control::cancel_engine(handle.id);
        clear_event_callback(&handle);
        crate::results::purge_engine(handle.id);
        handle.engine.shutdown();
        FMF_OK
    })
}

pub(crate) fn engine(h: *mut c_void) -> Result<Arc<EngineHandle>, i32> {
    if h.is_null() {
        set_error("null engine handle");
        return Err(FMF_E_INVALID_ARG);
    }
    let id = opaque::id_of(h);
    ENGINES.lock().get(&id).cloned().ok_or_else(|| {
        set_error(format!("unknown or destroyed engine handle: {id}"));
        FMF_E_INVALID_ARG
    })
}

fn take_engine(h: *mut c_void) -> Result<Arc<EngineHandle>, i32> {
    if h.is_null() {
        set_error("null engine handle");
        return Err(FMF_E_INVALID_ARG);
    }
    let id = opaque::id_of(h);
    ENGINES.lock().remove(&id).ok_or_else(|| {
        set_error(format!("unknown or already destroyed engine handle: {id}"));
        FMF_E_INVALID_ARG
    })
}
