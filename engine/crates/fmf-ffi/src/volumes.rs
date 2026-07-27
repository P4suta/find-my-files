use std::ffi::{c_char, c_void};

use fmf_contract::volume::encode_label;
use fmf_core::engine::Engine;

use crate::error::{guard, set_error, utf8_arg};
use crate::handle::engine;
use crate::{FMF_E_INVALID_ARG, FMF_E_IO, FMF_OK};

// ── Volumes & indexing ──────────────────────────────────────────────────

// The status POD radiates from the contract (ADR-0018).
pub use fmf_contract::pod::FmfVolumeStatus;

/// Enumerate the NTFS volumes available for indexing, writing up to `cap`
/// entries into `buf` and the total count into `count`.
///
/// # Safety
///
/// `count` must be aligned and writable as one `u32`. When `buf` is non-null,
/// it must be aligned and writable for `cap` consecutive `FmfVolumeStatus`
/// values. `count` is initialized to zero before handle validation. Those
/// regions must not overlap and must remain valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_list_volumes(
    h: *mut c_void,
    buf: *mut FmfVolumeStatus,
    cap: u32,
    count: *mut u32,
) -> i32 {
    guard(|| {
        if count.is_null() {
            set_error("fmf_list_volumes requires a non-null count pointer");
            return FMF_E_INVALID_ARG;
        }
        unsafe { *count = 0 };
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };
        let vols = Engine::list_ntfs_volumes();
        let Ok(total) = u32::try_from(vols.len()) else {
            set_error("volume count exceeds the ABI's u32 length limit");
            return FMF_E_IO;
        };
        unsafe { *count = total };
        if !buf.is_null() {
            for (i, v) in vols.iter().take(cap as usize).enumerate() {
                unsafe {
                    *buf.add(i) = FmfVolumeStatus {
                        label: encode_label(v),
                        state: 0,
                        _pad: 0,
                        entries: 0,
                    };
                }
            }
        }
        FMF_OK
    })
}

/// Begin indexing the `n` volume labels pointed to by `volumes` on the engine
/// behind handle `h`.
///
/// # Safety
///
/// For nonzero `n`, `volumes` must address `n` readable, aligned C-string
/// pointers. Every element must point to readable, NUL-terminated UTF-8 and
/// the array and strings must remain immutable for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_start(
    h: *mut c_void,
    volumes: *const *const c_char,
    n: u32,
) -> i32 {
    guard(|| {
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };
        if volumes.is_null() && n > 0 {
            set_error("fmf_index_start volumes is null while n is nonzero");
            return FMF_E_INVALID_ARG;
        }
        if n > fmf_contract::limits::MAX_VOLUMES {
            set_error(format!(
                "fmf_index_start volume count {n} exceeds the contract maximum {}",
                fmf_contract::limits::MAX_VOLUMES
            ));
            return FMF_E_INVALID_ARG;
        }
        let mut labels = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            match unsafe { utf8_arg(*volumes.add(i)) } {
                Ok(s) => labels.push(s),
                Err(c) => return c,
            }
        }
        match handle.engine.index_start(&labels) {
            Ok(()) => FMF_OK,
            Err(error) => {
                set_error(format!("fmf_index_start rejected: {error}"));
                FMF_E_INVALID_ARG
            }
        }
    })
}

/// Report per-volume indexing status for the engine behind handle `h`, writing
/// up to `cap` entries into `buf` and the total count into `count`.
///
/// # Safety
///
/// `count` must be aligned and writable as one `u32`. When `buf` is non-null,
/// it must be aligned and writable for `cap` consecutive `FmfVolumeStatus`
/// values. `count` is initialized to zero before handle validation. Those
/// regions must not overlap and must remain valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_status(
    h: *mut c_void,
    buf: *mut FmfVolumeStatus,
    cap: u32,
    count: *mut u32,
) -> i32 {
    guard(|| {
        if count.is_null() {
            set_error("fmf_index_status requires a non-null count pointer");
            return FMF_E_INVALID_ARG;
        }
        unsafe { *count = 0 };
        let handle = match engine(h) {
            Ok(e) => e,
            Err(c) => return c,
        };
        let _active = match handle.enter() {
            Ok(active) => active,
            Err(c) => return c,
        };
        let status = handle.engine.status();
        let Ok(total) = u32::try_from(status.len()) else {
            set_error("index status count exceeds the ABI's u32 length limit");
            return FMF_E_IO;
        };
        unsafe { *count = total };
        if !buf.is_null() {
            for (i, (label, phase, entries)) in status.iter().take(cap as usize).enumerate() {
                // VolumeState is the contract enum (repr u32) — no mapping.
                let state = *phase as u32;
                unsafe {
                    *buf.add(i) = FmfVolumeStatus {
                        label: encode_label(label),
                        state,
                        _pad: 0,
                        entries: *entries,
                    };
                }
            }
        }
        FMF_OK
    })
}
