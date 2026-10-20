//! NTFS acquisition errors and the deterministic byte grammar.
pub use fmf_core::ondisk::ntfs::*;
use thiserror::Error;

/// Why raw NTFS access or decoding failed.
#[derive(Debug, Error)]
pub enum NtfsError {
    /// The raw volume handle requires privileges this process does not hold.
    #[error("raw volume access requires an elevated service")]
    Elevation,
    /// A device read or FSCTL failed.
    #[error("NTFS I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// $MFT record 0 lacked an attribute the scanner needs.
    #[error("missing required MFT attribute: {0}")]
    MissingMftAttribute(String),
    /// The bytes on disk do not describe a structure this decoder accepts.
    #[error("invalid NTFS data: {0}")]
    InvalidData(&'static str),
}
