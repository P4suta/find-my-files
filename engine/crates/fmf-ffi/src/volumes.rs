use std::ffi::{c_char, c_void};

use fmf_core::engine::{Engine, VolumePhase};

use crate::error::{guard, utf8_arg};
use crate::events::volume_bytes;
use crate::handle::engine;
use crate::{FMF_E_INVALID_ARG, FMF_OK};

// ── Volumes & indexing ──────────────────────────────────────────────────

#[repr(C)]
pub struct FmfVolumeStatus {
    pub label: [u8; 16],
    pub state: u32, // 0=Scanning 1=Ready 2=Rescanning 3=Failed
    pub _pad: u32,
    pub entries: u64,
}

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
                        label: volume_bytes(v),
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
                let state = match phase {
                    VolumePhase::Scanning => 0,
                    VolumePhase::Ready => 1,
                    VolumePhase::Rescanning => 2,
                    VolumePhase::Failed => 3,
                };
                unsafe {
                    *buf.add(i) = FmfVolumeStatus {
                        label: volume_bytes(label),
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
