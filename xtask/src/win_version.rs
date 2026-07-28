//! Native, shell-free reader for Win32 `VERSIONINFO` string resources.
//!
//! Release verification must not depend on PowerShell formatting, code pages,
//! profiles, or executable lookup. The supported Windows path calls version.dll
//! directly; non-Windows hosts fail closed if asked to verify a Windows bundle.

use anyhow::{bail, Result};
use std::path::Path;

#[cfg(windows)]
use std::{ffi::c_void, ptr};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};

#[derive(Debug, PartialEq, Eq)]
pub struct VersionInfo {
    pub product_version: String,
    pub file_version: String,
}

#[cfg(windows)]
pub fn read(path: &Path) -> Result<VersionInfo> {
    use anyhow::Context as _;
    use std::os::windows::ffi::OsStrExt as _;

    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut ignored_handle = 0u32;
    // SAFETY: `wide_path` is NUL-terminated and `ignored_handle` is writable.
    let size = unsafe { GetFileVersionInfoSizeW(wide_path.as_ptr(), &raw mut ignored_handle) };
    if size == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("read VERSIONINFO size from {}", path.display()));
    }
    let size_usize = usize::try_from(size).context("VERSIONINFO size does not fit usize")?;
    let mut data = vec![0u8; size_usize];
    // SAFETY: the API receives the same valid path and a buffer of exactly the
    // byte length returned by `GetFileVersionInfoSizeW`.
    let loaded = unsafe {
        GetFileVersionInfoW(
            wide_path.as_ptr(),
            0,
            size,
            data.as_mut_ptr().cast::<c_void>(),
        )
    };
    if loaded == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("read VERSIONINFO from {}", path.display()));
    }

    let mut translations = query_translations(&data);
    // winresource emits US-English Unicode today. Keep it as a standards-based
    // fallback for a malformed/missing Translation table, while preferring every
    // translation the resource itself declares.
    if !translations.contains(&(0x0409, 0x04b0)) {
        translations.push((0x0409, 0x04b0));
    }
    for (language, codepage) in translations {
        let prefix = format!(r"\StringFileInfo\{language:04X}{codepage:04X}");
        let product = query_string(&data, &format!(r"{prefix}\ProductVersion"));
        let file = query_string(&data, &format!(r"{prefix}\FileVersion"));
        if let (Some(product_version), Some(file_version)) = (product, file) {
            return Ok(VersionInfo {
                product_version,
                file_version,
            });
        }
    }

    bail!(
        "{} has no readable ProductVersion/FileVersion string pair",
        path.display()
    )
}

#[cfg(windows)]
fn query_translations(data: &[u8]) -> Vec<(u16, u16)> {
    let subblock = wide(r"\VarFileInfo\Translation");
    let mut buffer = ptr::null_mut::<c_void>();
    let mut byte_len = 0u32;
    // SAFETY: `data` remains alive, `subblock` is NUL-terminated, and both
    // out-pointers are writable for the duration of the call.
    let found = unsafe {
        VerQueryValueW(
            data.as_ptr().cast::<c_void>(),
            subblock.as_ptr(),
            &raw mut buffer,
            &raw mut byte_len,
        )
    };
    if found == 0 || buffer.is_null() || byte_len < 4 {
        return Vec::new();
    }
    let Ok(word_len) = usize::try_from(byte_len / 2) else {
        return Vec::new();
    };
    // SAFETY: version.dll returned a buffer inside `data` with `byte_len`
    // bytes. Translation entries are two aligned u16 values.
    let words = unsafe { std::slice::from_raw_parts(buffer.cast::<u16>(), word_len) };
    words
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

#[cfg(windows)]
fn query_string(data: &[u8], subblock: &str) -> Option<String> {
    let subblock = wide(subblock);
    let mut buffer = ptr::null_mut::<c_void>();
    let mut char_len = 0u32;
    // SAFETY: `data` remains alive, `subblock` is NUL-terminated, and both
    // out-pointers are writable for the duration of the call.
    let found = unsafe {
        VerQueryValueW(
            data.as_ptr().cast::<c_void>(),
            subblock.as_ptr(),
            &raw mut buffer,
            &raw mut char_len,
        )
    };
    if found == 0 || buffer.is_null() || char_len == 0 {
        return None;
    }
    let char_len = usize::try_from(char_len).ok()?;
    // SAFETY: for a string query, version.dll returns `char_len` UTF-16 code
    // units inside the still-live VERSIONINFO buffer.
    let value = unsafe { std::slice::from_raw_parts(buffer.cast::<u16>(), char_len) };
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    let value = String::from_utf16(&value[..end]).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[cfg(not(windows))]
pub fn read(path: &Path) -> Result<VersionInfo> {
    bail!(
        "cannot verify Windows VERSIONINFO for {} on a non-Windows host",
        path.display()
    )
}
