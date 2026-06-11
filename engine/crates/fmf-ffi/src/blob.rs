use std::ffi::c_void;

use crate::error::{guard, set_error};
use crate::handle::engine;
use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_OK};

// ── JSON blobs (stats / traces) ─────────────────────────────────────────

// The blob POD radiates from the contract (ADR-0018).
pub use fmf_contract::pod::FmfBlob;

#[repr(C)]
struct BlobOwned {
    blob: FmfBlob, // must stay first: its address is the handle
    bytes: Vec<u8>,
}

pub(crate) fn blob_from_json(json: String) -> *mut FmfBlob {
    let bytes = json.into_bytes();
    let mut owned = Box::new(BlobOwned {
        blob: FmfBlob {
            data: std::ptr::null(),
            len: bytes.len() as u32,
            _pad: 0,
        },
        bytes,
    });
    owned.blob.data = owned.bytes.as_ptr();
    Box::into_raw(owned).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_blob_free(p: *mut FmfBlob) -> i32 {
    guard(|| {
        if !p.is_null() {
            drop(unsafe { Box::from_raw(p.cast::<BlobOwned>()) });
        }
        FMF_OK
    })
}

/// Full observability snapshot (recent query traces, latency histogram,
/// USN feed, per-volume index stats) as JSON.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_engine_stats(h: *mut c_void, out: *mut *mut FmfBlob) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if out.is_null() {
            return FMF_E_INVALID_ARG;
        }
        match serde_json::to_string(&handle.engine.metrics_snapshot()) {
            Ok(json) => {
                unsafe { *out = blob_from_json(json) };
                FMF_OK
            }
            Err(e) => {
                set_error(e.to_string());
                FMF_E_IO
            }
        }
    })
}
