//! Authenticode-stable SHA-256 identity for a PE image.
//!
//! `ImageGetDigestStream` returns the bytes Windows considers part of the PE
//! image while omitting the mutable certificate table. The digest therefore
//! stays identical before and after release signing and can be embedded into
//! the app before both files are signed.

use anyhow::{bail, Result};
use std::path::Path;

#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::fmt::Write as _;
#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::sync::Mutex;

#[cfg(windows)]
const DIGEST_LEVEL_ALL: u32 = 0x01 | 0x02 | 0x04;

#[cfg(windows)]
static IMAGEHLP_LOCK: Mutex<()> = Mutex::new(());

#[cfg(windows)]
#[link(name = "imagehlp")]
unsafe extern "system" {
    fn ImageGetDigestStream(
        file_handle: *mut std::ffi::c_void,
        digest_level: u32,
        digest_function: Option<
            unsafe extern "system" fn(digest_handle: usize, data: *const u8, length: u32) -> i32,
        >,
        digest_handle: usize,
    ) -> i32;
}

#[cfg(windows)]
unsafe extern "system" fn append_digest(digest_handle: usize, data: *const u8, length: u32) -> i32 {
    if digest_handle == 0 || (data.is_null() && length != 0) {
        return 0;
    }
    // SAFETY: `digest_handle` points at the stack-owned hasher for the entire
    // synchronous ImageGetDigestStream call. ImageHlp supplies `length`
    // readable bytes for the duration of this callback.
    let hasher = unsafe { &mut *(digest_handle as *mut Sha256) };
    let bytes = if length == 0 {
        &[]
    } else {
        // SAFETY: justified above; u32 length is representable on Windows x64.
        unsafe { std::slice::from_raw_parts(data, length as usize) }
    };
    hasher.update(bytes);
    1
}

/// Returns the lowercase SHA-256 of the PE digest stream Windows signs.
///
/// # Errors
///
/// Fails when the file cannot be opened, is not a valid PE image, `ImageHlp`
/// rejects it, or the process-global `ImageHlp` serialization lock is poisoned.
#[cfg(windows)]
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open PE image {}", path.display()))?;
    let _guard = IMAGEHLP_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("ImageHlp digest lock was poisoned"))?;
    let mut hasher = Sha256::new();
    // SAFETY: the file handle and stack-owned hasher remain live for the
    // synchronous call; the callback validates its inputs.
    let ok = unsafe {
        ImageGetDigestStream(
            file.as_raw_handle().cast(),
            DIGEST_LEVEL_ALL,
            Some(append_digest),
            std::ptr::from_mut(&mut hasher).addr(),
        )
    };
    if ok == 0 {
        bail!(
            "ImageGetDigestStream rejected {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    Ok(encoded)
}

/// PE publishing is Windows-only. Keeping a stub lets the xtask workspace and
/// its pure tests compile on Ubuntu CI without pretending a raw-file hash has
/// the same semantics.
#[cfg(not(windows))]
pub fn sha256_file(path: &Path) -> Result<String> {
    bail!(
        "PE image digest requires Windows ImageHlp: {}",
        path.display()
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::sha256_file;

    #[test]
    fn pe_digest_is_stable_for_repeated_reads() {
        let executable = std::env::current_exe().expect("current test executable");
        let first = sha256_file(&executable).expect("first digest");
        let second = sha256_file(&executable).expect("second digest");
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
