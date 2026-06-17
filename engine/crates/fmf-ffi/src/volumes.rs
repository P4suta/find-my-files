use std::ffi::{c_char, c_void};

use fmf_contract::volume::encode_label;
use fmf_core::engine::Engine;

use crate::error::{guard, utf8_arg};
use crate::handle::engine;
use crate::{FMF_E_INVALID_ARG, FMF_OK};

// ── Volumes & indexing ──────────────────────────────────────────────────

// The status POD radiates from the contract (ADR-0018).
pub use fmf_contract::pod::FmfVolumeStatus;

/// Enumerate the NTFS volumes available for indexing, writing up to `cap`
/// entries into `buf` and the total count into `count`. Safety: see docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_list_volumes(
    _h: *mut c_void,
    buf: *mut FmfVolumeStatus,
    cap: u32,
    count: *mut u32,
) -> i32 {
    guard(|| {
        if count.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let vols = Engine::list_ntfs_volumes();
        unsafe { *count = vols.len() as u32 };
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
/// behind handle `h`. Safety: see docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_start(
    h: *mut c_void,
    volumes: *const *const c_char,
    n: u32,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if volumes.is_null() && n > 0 {
            return FMF_E_INVALID_ARG;
        }
        let mut labels = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            match unsafe { utf8_arg(*volumes.add(i)) } {
                Ok(s) => labels.push(s.to_string()),
                Err(c) => return c,
            }
        }
        handle.engine.index_start(&labels);
        FMF_OK
    })
}

/// Begin a non-elevated scope-mode index over `n` absolute root paths, pruning
/// any subtree under one of the `m` `excludes` paths at walk time (ADR-0025).
///
/// Mirrors [`fmf_index_start`] but routes `roots`/`excludes` to
/// `Engine::index_start_scope` (folder-walk + watcher, ADR-0024); the host must
/// have created the engine on a per-user (`%LOCALAPPDATA%`) index dir. Scope
/// mode is FFI-only and co-shipped with this DLL, so the signature is extended
/// in place (no POD layout change, `ABI_VERSION` unchanged). Safety: see
/// docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_start_scope(
    h: *mut c_void,
    roots: *const *const c_char,
    n: u32,
    excludes: *const *const c_char,
    m: u32,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if (roots.is_null() && n > 0) || (excludes.is_null() && m > 0) {
            return FMF_E_INVALID_ARG;
        }
        let mut paths = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            match unsafe { utf8_arg(*roots.add(i)) } {
                Ok(s) => paths.push(s.to_string()),
                Err(c) => return c,
            }
        }
        let mut excl = Vec::with_capacity(m as usize);
        for i in 0..m as usize {
            match unsafe { utf8_arg(*excludes.add(i)) } {
                Ok(s) => excl.push(s.to_string()),
                Err(c) => return c,
            }
        }
        handle.engine.index_start_scope(&paths, &excl);
        FMF_OK
    })
}

/// Report per-volume indexing status for the engine behind handle `h`, writing
/// up to `cap` entries into `buf` and the total count into `count`. Safety: see docs/ARCHITECTURE.md.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fmf_index_status(
    h: *mut c_void,
    buf: *mut FmfVolumeStatus,
    cap: u32,
    count: *mut u32,
) -> i32 {
    guard(|| {
        let handle = match unsafe { engine(h) } {
            Ok(e) => e,
            Err(c) => return c,
        };
        if count.is_null() {
            return FMF_E_INVALID_ARG;
        }
        let status = handle.engine.status();
        unsafe { *count = status.len() as u32 };
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
