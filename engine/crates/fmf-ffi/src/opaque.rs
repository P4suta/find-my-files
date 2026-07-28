//! Process-wide opaque-handle identifiers.
//!
//! C callers transport these values as pointers but Rust never dereferences
//! them. A single monotonic namespace is shared by engine and result handles,
//! preventing both ABA reuse and accidental cross-kind aliasing.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::FMF_E_IO;
use crate::error::set_error;

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

pub fn next_id(kind: &str) -> Result<usize, i32> {
    NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| {
            set_error(format!("{kind} handle id space exhausted"));
            FMF_E_IO
        })
}

pub const fn to_ptr(id: usize) -> *mut c_void {
    std::ptr::without_provenance_mut(id)
}

pub fn id_of(ptr: *mut c_void) -> usize {
    ptr.addr()
}
