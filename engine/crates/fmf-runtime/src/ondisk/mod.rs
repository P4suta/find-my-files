//! NTFS byte grammar and stream acquisition.
pub mod attribute_list;
/// NTFS update-sequence repair.
pub use fmf_core::ondisk::fixup;
pub mod ntfs;
/// Complete file-record attribute validation.
pub use fmf_core::ondisk::record;
