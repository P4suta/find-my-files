use std::cell::RefCell;
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{FMF_E_INVALID_ARG, FMF_E_PANIC, FMF_OK};

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn set_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

/// Full cause chain — fmf-core::diag owns the single implementation
/// (4 KiB cap included; shared with the pipe error responses — ADR-0018).
pub(crate) use fmf_core::diag::error_chain;

pub(crate) fn guard<F: FnOnce() -> i32>(f: F) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => {
            set_error("panic inside fmf_engine");
            FMF_E_PANIC
        }
    }
}

pub(crate) unsafe fn utf8_arg<'a>(p: *const c_char) -> Result<&'a str, i32> {
    if p.is_null() {
        set_error("null string argument");
        return Err(FMF_E_INVALID_ARG);
    }
    unsafe { CStr::from_ptr(p) }.to_str().map_err(|_| {
        set_error("argument is not valid UTF-8");
        FMF_E_INVALID_ARG
    })
}

// ── Diagnostics ─────────────────────────────────────────────────────────

/// Copies the thread-local detail message. `len` is in/out (capacity →
/// written bytes, excluding the NUL this function appends when room allows).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_last_error(buf: *mut u8, len: *mut u32) -> i32 {
    if len.is_null() {
        return FMF_E_INVALID_ARG;
    }
    LAST_ERROR.with(|e| {
        let msg = e.borrow();
        let bytes = msg.as_bytes();
        let cap = unsafe { *len } as usize;
        let n = bytes.len().min(cap.saturating_sub(1));
        if !buf.is_null() && cap > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
                *buf.add(n) = 0;
            }
        }
        unsafe { *len = n as u32 };
    });
    FMF_OK
}
