use std::cell::RefCell;
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_E_PANIC, FMF_OK};

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn set_error(msg: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into());
}

fn abi_len(len: usize) -> Option<u32> {
    u32::try_from(len).ok()
}

pub(crate) fn checked_abi_len(len: usize, value: &str) -> Result<u32, i32> {
    abi_len(len).ok_or_else(|| {
        set_error(format!("{value} exceeds the ABI's u32 length limit"));
        FMF_E_IO
    })
}

fn clear_error() {
    LAST_ERROR.with(|e| e.borrow_mut().clear());
}

/// Full cause chain — `fmf-core::diag` owns the single implementation
/// (4 KiB cap included; shared with the pipe error responses — ADR-0018).
pub(crate) use fmf_core::diag::error_chain;

pub(crate) fn guard<F: FnOnce() -> i32>(f: F) -> i32 {
    // Error detail belongs to one call. A later success or a failure branch
    // that has no richer detail must never expose stale text from an earlier
    // operation on the same thread.
    clear_error();
    if let Ok(code) = catch_unwind(AssertUnwindSafe(f)) {
        code
    } else {
        set_error("panic inside fmf_engine");
        FMF_E_PANIC
    }
}

/// Run a cleanup entry point without erasing the diagnostic from the
/// operation it cleans up.
///
/// Query controls must be freed after `fmf_query` even when that query failed.
/// Both calls run on the caller thread, so an ordinary [`guard`] around the
/// successful free would clear the query's thread-local error before the host
/// can retrieve it. A cleanup failure still replaces the prior detail with its
/// own error.
pub(crate) fn guard_cleanup<F: FnOnce() -> i32>(f: F) -> i32 {
    let previous = LAST_ERROR.with(|error| error.borrow().clone());
    let code = guard(f);
    if code == FMF_OK {
        set_error(previous);
    }
    code
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

/// Copies the thread-local detail message.
///
/// `len` is in/out (capacity → required/written bytes, excluding NUL). A null
/// buffer probes the size; a real buffer must hold `required + 1` bytes or the
/// call fails without a partial write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_last_error(buf: *mut u8, len: *mut u32) -> i32 {
    if len.is_null() {
        return FMF_E_INVALID_ARG;
    }
    LAST_ERROR.with(|e| {
        let msg = e.borrow();
        let bytes = msg.as_bytes();
        let required = bytes.len();
        let cap = unsafe { *len } as usize;
        let Some(required_u32) = abi_len(required) else {
            // `LAST_ERROR` is borrowed here, so do not recursively call
            // `set_error`. Saturate the observable size and fail closed.
            unsafe { *len = u32::MAX };
            return FMF_E_INVALID_ARG;
        };
        unsafe { *len = required_u32 };
        if buf.is_null() {
            return FMF_OK;
        }
        if cap <= required {
            return FMF_E_INVALID_ARG;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, required);
            *buf.add(required) = 0;
        }
        FMF_OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_lengths_are_checked_before_narrowing() {
        assert_eq!(abi_len(u32::MAX as usize), Some(u32::MAX));
        #[cfg(target_pointer_width = "64")]
        {
            let too_large = u32::MAX as usize + 1;
            assert_eq!(abi_len(too_large), None);
            assert_eq!(checked_abi_len(too_large, "test payload"), Err(FMF_E_IO));
        }
    }
}
