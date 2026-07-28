//! Process-wide owner IDs for FFI allocations.
//!
//! Pages and JSON blobs share one monotonic namespace. The C caller receives
//! the ID in the descriptor and returns only that ID to the matching free
//! function; Rust never reconstructs ownership from a foreign address.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::FMF_E_IO;
use crate::error::set_error;

/// Zero is the no-allocation sentinel accepted as a free no-op.
static NEXT_OWNER_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_owner_id(kind: &str) -> Result<u64, i32> {
    NEXT_OWNER_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| {
            set_error(format!("{kind} allocation owner id space exhausted"));
            FMF_E_IO
        })
}
