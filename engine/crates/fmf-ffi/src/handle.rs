use std::ffi::{c_char, c_void};
use std::sync::Arc;

use fmf_core::engine::{Engine, EngineConfig, EngineCreateError};

use crate::error::{guard, set_error, utf8_arg};
use crate::events::CallbackSink;
use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_LOCKED, FMF_OK};

// ── Handles ─────────────────────────────────────────────────────────────

pub(crate) struct EngineHandle {
    pub(crate) engine: Arc<Engine>,
    // Keeps the registered callback (and its user pointer) alive.
    pub(crate) _sink_keepalive: parking_lot::Mutex<Option<Arc<CallbackSink>>>,
}

#[unsafe(no_mangle)]
pub extern "C" fn fmf_abi_version() -> u32 {
    fmf_contract::versions::ABI_VERSION
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

        // Diagnostics first: everything after this point is observable
        // (file log, diag ring, ENGINE_ERROR events). Resolution rule and
        // bootstrap live in fmf-core::diag — the single home (ADR-0018).
        let log_dir = fmf_core::diag::resolve_log_dir(
            parsed
                .get("log_dir")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from),
        );
        let log_level = parsed
            .get("log_level")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        fmf_core::diag::init_diag(Some(&log_dir), log_level);

        let engine = match Engine::new(EngineConfig {
            index_dir: index_dir.into(),
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
        let handle = Box::new(EngineHandle {
            engine,
            _sink_keepalive: parking_lot::Mutex::new(None),
        });
        unsafe { *out = Box::into_raw(handle).cast() };
        FMF_OK
    })
}

/// Saves every Ready, dirty volume now (docs/ARCHITECTURE.md fmf_flush).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_flush(h: *mut c_void) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        handle.engine.flush();
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

pub(crate) unsafe fn engine<'a>(h: *mut c_void) -> Result<&'a EngineHandle, i32> {
    if h.is_null() {
        set_error("null engine handle");
        return Err(FMF_E_INVALID_ARG);
    }
    Ok(unsafe { &*h.cast::<EngineHandle>() })
}
