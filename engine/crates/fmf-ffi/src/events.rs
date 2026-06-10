use std::ffi::c_void;
use std::sync::Arc;

use fmf_core::engine::EngineEvent;

use crate::FMF_OK;
use crate::error::guard;
use crate::handle::engine;

// ── Events ──────────────────────────────────────────────────────────────

pub const FMF_EVENT_PROGRESS: u32 = 1;
pub const FMF_EVENT_VOLUME_READY: u32 = 2;
pub const FMF_EVENT_INDEX_CHANGED: u32 = 3;
pub const FMF_EVENT_RESCAN_STARTED: u32 = 4;
pub const FMF_EVENT_VOLUME_FAILED: u32 = 5;
/// entries = severity (1=warn 2=error 3=panic); details via fmf_engine_stats.
pub const FMF_EVENT_ENGINE_ERROR: u32 = 6;

/// POD event payload. `volume` is NUL-terminated UTF-8 ("C:").
#[repr(C)]
pub struct FmfEvent {
    pub kind: u32,
    pub _pad: u32,
    pub entries: u64,
    pub volume: [u8; 16],
}

pub type FmfEventCb = Option<unsafe extern "C" fn(ev: *const FmfEvent, user: *mut c_void)>;

pub(crate) struct CallbackSink {
    cb: unsafe extern "C" fn(*const FmfEvent, *mut c_void),
    user: *mut c_void,
}
// Contract: the callback must be callable from any thread; the user pointer
// is treated as an opaque token.
unsafe impl Send for CallbackSink {}
unsafe impl Sync for CallbackSink {}

pub(crate) fn volume_bytes(label: &str) -> [u8; 16] {
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
                            EngineEvent::EngineError { severity, volume } => {
                                (FMF_EVENT_ENGINE_ERROR, volume, *severity)
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
