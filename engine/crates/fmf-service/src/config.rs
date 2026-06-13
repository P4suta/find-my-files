//! Machine-wide service config: `%ProgramData%\find-my-files\service.json`
//! (docs/ARCHITECTURE.md「Pipe プロトコル」§マシン単位設定). Owned by the
//! service; `install` (P4) seeds it with the captured user SID.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceConfig {
    /// Drive labels to index. Empty = all fixed NTFS volumes at startup.
    pub volumes: Vec<String>,
    pub log_level: String,
    pub flush_interval_secs: u64,
    /// SIDs allowed to connect (P4: SDDL + connect-time token check).
    pub authorized_sids: Vec<String>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            volumes: Vec::new(),
            log_level: "info".to_string(),
            flush_interval_secs: 300,
            authorized_sids: Vec::new(),
        }
    }
}

#[must_use]
pub fn default_data_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    Path::new(&base).join("find-my-files")
}

impl ServiceConfig {
    /// Missing file → defaults. A corrupt file is a loud warn + defaults —
    /// the service must come up searchable, not die on a bad byte.
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "service.json unreadable — defaults");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "service.json unreadable — defaults");
                Self::default()
            }
        }
    }

    /// # Errors
    /// Propagates the I/O error from creating the parent directory or writing
    /// the file.
    ///
    /// # Panics
    /// Panics if serializing the config to JSON fails — unreachable for this
    /// plain `#[derive(Serialize)]` struct.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).expect("config serializes");
        std::fs::write(path, json)
    }
}
