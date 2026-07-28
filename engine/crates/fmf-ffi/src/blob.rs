use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::c_void;
use std::sync::LazyLock;

use parking_lot::Mutex;

use crate::allocation::next_owner_id;
use crate::error::{checked_abi_len, guard, set_error};
use crate::handle::engine;
use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_OK};

// ── JSON blobs (stats / traces) ─────────────────────────────────────────

// The blob POD radiates from the contract (ADR-0018).
pub use fmf_contract::pod::FmfBlob;

#[repr(C)]
struct BlobOwned {
    blob: FmfBlob, // published descriptor; the Box keeps its address stable
    bytes: Vec<u8>,
}

// SAFETY: `blob.data` is either null or points into `bytes`. Moving the Box
// between registry buckets/threads does not move either allocation, and the
// owner is never mutated after publication. The registry mutex serializes
// ownership transfer and removal; callers must still not read while freeing.
#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the raw descriptor pointer targets this Box's immutable Vec allocation"
)]
unsafe impl Send for BlobOwned {}

static BLOBS: LazyLock<Mutex<HashMap<u64, Box<BlobOwned>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn blob_from_json(json: String) -> Result<*mut FmfBlob, i32> {
    let bytes = json.into_bytes();
    let len = checked_abi_len(bytes.len(), "JSON blob")?;
    let owner_id = next_owner_id("JSON blob")?;
    let mut owned = Box::new(BlobOwned {
        blob: FmfBlob {
            data: std::ptr::null(),
            len,
            _pad: 0,
            owner_id,
        },
        bytes,
    });
    owned.blob.data = if owned.bytes.is_empty() {
        std::ptr::null()
    } else {
        owned.bytes.as_ptr()
    };
    let mut blobs = BLOBS.lock();
    match blobs.entry(owner_id) {
        // Derive the pointer from the stored box, after the move — see the
        // same reasoning in `results.rs`.
        Entry::Vacant(entry) => Ok(std::ptr::from_mut(&mut entry.insert(owned).blob)),
        Entry::Occupied(_) => {
            set_error(format!(
                "duplicate JSON blob allocation owner id: {owner_id}"
            ));
            Err(FMF_E_IO)
        }
    }
}

/// Frees the JSON blob identified by the monotonic owner ID returned in its
/// descriptor. ID zero is a no-op; unknown, wrong-kind, stale, and already
/// freed IDs are rejected without touching foreign memory.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_blob_free(owner_id: u64) -> i32 {
    guard(|| {
        if owner_id == 0 {
            return FMF_OK;
        }
        if BLOBS.lock().remove(&owner_id).is_some() {
            FMF_OK
        } else {
            set_error(format!(
                "unknown, wrong-kind, or already freed JSON blob owner id: {owner_id}"
            ));
            FMF_E_INVALID_ARG
        }
    })
}

/// Full observability snapshot (recent query traces, latency histogram,
/// USN feed, per-volume index stats) as JSON.
///
/// # Safety
///
/// `out` must be aligned and writable as one `*mut FmfBlob` for this call. It
/// is initialized to null; on success the returned descriptor and its bytes
/// remain borrowed from the native registry until its `owner_id` is passed
/// exactly once to `fmf_blob_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_engine_stats(h: *mut c_void, out: *mut *mut FmfBlob) -> i32 {
    guard(|| {
        if out.is_null() {
            set_error("fmf_engine_stats requires a non-null out pointer");
            return FMF_E_INVALID_ARG;
        }
        unsafe { *out = std::ptr::null_mut() };
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };
        match serde_json::to_string(&handle.engine.metrics_snapshot()) {
            Ok(json) => match blob_from_json(json) {
                Ok(blob) => {
                    unsafe { *out = blob };
                    FMF_OK
                }
                Err(code) => code,
            },
            Err(e) => {
                set_error(e.to_string());
                FMF_E_IO
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{blob_from_json, fmf_blob_free};
    use crate::{FMF_E_INVALID_ARG, FMF_OK};

    #[test]
    fn blob_registry_rejects_double_free_and_forged_ids() {
        let blob = blob_from_json("{}".to_owned()).expect("test blob allocation");
        let owner_id = unsafe { (*blob).owner_id };
        assert_ne!(owner_id, 0);
        assert_eq!(fmf_blob_free(owner_id), FMF_OK);
        assert_eq!(fmf_blob_free(owner_id), FMF_E_INVALID_ARG);

        assert_eq!(fmf_blob_free(u64::MAX), FMF_E_INVALID_ARG);
    }
}
