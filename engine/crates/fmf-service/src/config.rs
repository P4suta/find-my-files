//! Machine-wide service config: `%ProgramData%\find-my-files\service.json`
//! (docs/ARCHITECTURE.md "Pipe protocol" §machine-wide settings).
//!
//! Owned by the service; `install` (P4) seeds it with the captured user SID.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_SERVICE_CONFIG_BYTES: u64 = 16 * 1024;
const MAX_AUTHORIZED_SIDS: usize = 2;

/// Machine-wide service settings persisted to `service.json`. Owned by the
/// service; omitted fields use [`Default`], while malformed files are rejected
/// by the installed-service path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceConfig {
    /// Tracing filter level for the engine log (e.g. `info`, `debug`).
    pub log_level: String,
    /// Interval between index flushes to disk, in seconds.
    pub flush_interval_secs: u64,
    /// Self-stop the service after this many seconds with no live pipe
    /// connection (on-demand lifecycle, ADR-0027). `0` disables idle-stop —
    /// the legacy "stay resident once started" behaviour. The clock starts
    /// only after the first client has connected and then dropped.
    pub idle_stop_secs: u64,
    /// The daily GC task (ADR-0027) uninstalls the service when it has not
    /// been used for this many days. `0` disables the GC.
    pub gc_max_idle_days: u64,
    /// SIDs allowed to connect (P4: SDDL + connect-time token check).
    pub authorized_sids: Vec<String>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            flush_interval_secs: 300,
            idle_stop_secs: 300,
            gc_max_idle_days: 7,
            authorized_sids: Vec::new(),
        }
    }
}

fn known_folder_path(id: &windows_sys::core::GUID, label: &str) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;

    let mut raw = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &raw mut raw) };
    if hr < 0 {
        return Err(std::io::Error::other(format!(
            "SHGetKnownFolderPath({label}) failed: HRESULT 0x{hr:08X}"
        )));
    }
    if raw.is_null() {
        return Err(std::io::Error::other(format!(
            "SHGetKnownFolderPath({label}) returned null"
        )));
    }
    let mut len = 0;
    unsafe {
        while *raw.add(len) != 0 {
            len += 1;
        }
    }
    let base = unsafe { OsString::from_wide(std::slice::from_raw_parts(raw, len)) };
    unsafe { CoTaskMemFree(raw.cast()) };
    Ok(PathBuf::from(base))
}

/// Resolves the machine-wide data directory without trusting inherited
/// environment variables across the UAC boundary.
///
/// # Errors
/// Returns an HRESULT-backed error if Windows cannot resolve `ProgramData`.
pub fn default_data_dir() -> std::io::Result<PathBuf> {
    use windows_sys::Win32::UI::Shell::FOLDERID_ProgramData;
    Ok(known_folder_path(&FOLDERID_ProgramData, "ProgramData")?.join("find-my-files"))
}

/// Resolves the trusted native System32 directory. Elevated child processes
/// must be launched from here, never by a PATH-searched executable name.
///
/// # Errors
/// Returns an HRESULT-backed error if Windows cannot resolve `System`.
pub fn system_dir() -> std::io::Result<PathBuf> {
    use windows_sys::Win32::UI::Shell::FOLDERID_System;
    known_folder_path(&FOLDERID_System, "System")
}

impl ServiceConfig {
    fn validate(&self) -> std::io::Result<()> {
        if !matches!(
            self.log_level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "log_level must be one of trace, debug, info, warn, error",
            ));
        }
        if self.flush_interval_secs < 10 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "flush_interval_secs must be at least 10",
            ));
        }
        if self.authorized_sids.len() > MAX_AUTHORIZED_SIDS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("authorized_sids has more than {MAX_AUTHORIZED_SIDS} entries"),
            ));
        }
        for (index, sid) in self.authorized_sids.iter().enumerate() {
            if !crate::security::is_canonical_sid(sid) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("authorized_sids[{index}] is not a canonical SID"),
                ));
            }
            if self.authorized_sids[..index].contains(sid) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("authorized_sids[{index}] is duplicated"),
                ));
            }
        }
        Ok(())
    }

    /// Reads and validates one persisted config.
    ///
    /// # Errors
    /// Returns `NotFound`, I/O, or `InvalidData` for malformed JSON.
    pub fn try_load(path: &Path) -> std::io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_SERVICE_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SERVICE_CONFIG_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("service.json exceeds {MAX_SERVICE_CONFIG_BYTES} bytes"),
            ));
        }
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        config.validate()?;
        Ok(config)
    }

    /// Console/dev policy: a missing or corrupt config warns and uses defaults.
    /// The installed SCM path must use [`Self::try_load`] and fail closed.
    pub fn load_or_default(path: &Path) -> Self {
        let (config, warning) = Self::load_or_default_with_error(path);
        if let Some(error) = warning {
            tracing::warn!(
                path = %path.display(),
                %error,
                "service.json unreadable — console defaults"
            );
        }
        config
    }

    /// Resolve console defaults without logging, for process entry points that
    /// must read `log_level` before diagnostics can be initialized. The caller
    /// records the returned error immediately after initializing diagnostics.
    #[must_use]
    pub fn load_or_default_with_error(path: &Path) -> (Self, Option<std::io::Error>) {
        match Self::try_load(path) {
            Ok(config) => (config, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(error) => (Self::default(), Some(error)),
        }
    }

    /// Writes through a sibling temporary file and atomically replaces the
    /// destination, so power loss cannot leave a partially serialized config.
    ///
    /// # Errors
    /// Propagates serialization, create/write/sync, cleanup, or replace errors.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        self.validate()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        match std::fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&json)?;
        file.sync_all()?;
        drop(file);
        if let Err(replace_error) = replace_file(&temporary, path) {
            return match std::fs::remove_file(&temporary) {
                Ok(()) => Err(replace_error),
                Err(cleanup_error) => Err(std::io::Error::new(
                    replace_error.kind(),
                    format!("{replace_error}; temporary-file cleanup also failed: {cleanup_error}"),
                )),
            };
        }
        Ok(())
    }
}

fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_load_rejects_corrupt_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("service.json");
        std::fs::write(&path, b"{").expect("write corrupt config");

        let error = ServiceConfig::try_load(&path).expect_err("corrupt JSON must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn required_load_rejects_unknown_or_obsolete_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("service.json");
        std::fs::write(&path, br#"{"directory_scan_fallback":true}"#)
            .expect("write obsolete config");

        let error =
            ServiceConfig::try_load(&path).expect_err("unknown config keys must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn load_and_save_reject_semantically_invalid_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("service.json");
        std::fs::write(&path, br#"{"log_level":"verbose"}"#).expect("write invalid level");

        let level_error =
            ServiceConfig::try_load(&path).expect_err("invalid log level must fail closed");
        assert_eq!(level_error.kind(), std::io::ErrorKind::InvalidData);
        let (fallback, warning) = ServiceConfig::load_or_default_with_error(&path);
        assert_eq!(fallback.log_level, ServiceConfig::default().log_level);
        assert_eq!(
            warning
                .expect("invalid console config must remain observable")
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        let invalid = ServiceConfig {
            flush_interval_secs: 9,
            ..ServiceConfig::default()
        };
        let flush_error = invalid
            .save(&path)
            .expect_err("sub-minimum flush interval must not be persisted");
        assert_eq!(flush_error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn load_rejects_oversized_or_unsafe_sid_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("service.json");
        std::fs::write(&path, vec![b' '; MAX_SERVICE_CONFIG_BYTES as usize + 1])
            .expect("write oversized config");
        assert_eq!(
            ServiceConfig::try_load(&path)
                .expect_err("oversized config must be bounded")
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        let mut invalid = ServiceConfig {
            authorized_sids: vec!["S-1-5-21-abc".into()],
            ..ServiceConfig::default()
        };
        assert_eq!(
            invalid
                .save(&path)
                .expect_err("non-canonical SID must not reach SDDL construction")
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        invalid.authorized_sids = vec!["S-1-5-21-1-2-3-1001".into(); 2];
        assert_eq!(
            invalid
                .save(&path)
                .expect_err("duplicate SID must be rejected")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn save_atomically_replaces_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("service.json");
        let mut config = ServiceConfig::default();
        config.authorized_sids.push("S-1-5-21-1-2-3-1001".into());

        config.save(&path).expect("first save");
        config.idle_stop_secs = 17;
        config.save(&path).expect("replace");

        let loaded = ServiceConfig::try_load(&path).expect("load replaced config");
        assert_eq!(loaded.idle_stop_secs, 17);
        assert!(!path.with_extension("json.tmp").exists());
    }
}
