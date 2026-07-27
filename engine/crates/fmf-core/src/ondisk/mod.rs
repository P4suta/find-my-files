//! The NTFS on-disk byte grammar: pure, platform-independent decoders for the
//! bytes an elevated raw-volume read hands back.
//!
//! Every byte reaching this module is untrusted — a crafted VHDX or USB stick
//! lets an attacker choose all of it, and `fmf-service` parses it as
//! `LocalSystem`. `#![forbid(unsafe_code)]` makes memory unsafety
//! unreachable, so the remaining hazard is a panic (out-of-bounds slice,
//! arithmetic overflow) taking that service down.
//!
//! Nothing here opens a handle, issues an FSCTL, or otherwise touches the OS:
//! acquisition lives in `scan::volume_io` and `usn::session`, both
//! `#[cfg(windows)]`. Keeping the grammar out of those Windows modules is what
//! lets `engine/fuzz` drive it under libFuzzer with the address sanitizer on
//! the Linux CI runner (ADR-0047).
//!
//! - [`ntfs`] — boot sector, file-record header, attribute headers, `$FILE_NAME`
//! - [`record`] — whole-attribute-chain validation for one fixed-up record
//! - [`attribute_list`] — `$ATTRIBUTE_LIST` entries and non-resident run maps
//! - [`fixup`] — the update-sequence array applied to a record buffer

#![forbid(unsafe_code)]

pub mod attribute_list;
pub mod fixup;
pub mod ntfs;
pub mod record;
