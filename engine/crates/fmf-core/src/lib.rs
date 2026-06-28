//! fmf-core — the entire engine logic of find-my-files.
//!
//! This crate is a pure library: the FFI surface (`fmf-ffi`) and the dev CLI
//! (`fmf-cli`) must not contain logic of their own. See docs/ARCHITECTURE.md
//! for the canonical contract this crate fulfills.

// Declared in dataflow order — reading order = the order data moves
// (ingest: mft/scan → usn → index; search: query → engine; cross-cutting
// last). Names are unchanged; only the narrative order is meaningful.
//
// mft / scan / engine read the $MFT/USN through ntfs-reader + windows-sys, so
// they are `#[cfg(windows)]`. The remaining modules are platform-independent and
// compile on Linux too (no pure module references a gated one — verified), which
// is what lets engine/fuzz fuzz the pure parsers under libFuzzer on Linux. The
// only Windows piece outside these modules — `query::dates::WindowsLocalResolver`
// — is already `#[cfg(windows)]` inside the (otherwise pure) query module.
#[cfg(windows)]
pub mod mft;
#[cfg(windows)]
pub mod scan;
pub mod usn;

pub mod index;

pub mod query;

#[cfg(windows)]
pub mod engine;

pub mod diag;
pub mod metrics;
pub mod wtf8;
