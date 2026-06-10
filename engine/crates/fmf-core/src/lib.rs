//! fmf-core — the entire engine logic of find-my-files.
//!
//! This crate is a pure library: the FFI surface (`fmf-ffi`) and the dev CLI
//! (`fmf-cli`) must not contain logic of their own. See docs/ARCHITECTURE.md
//! for the canonical contract this crate fulfills.

pub mod engine;
pub mod index;
pub mod metrics;
pub mod mft;
pub mod query;
pub mod scan;
pub mod usn;
pub mod wtf8;
