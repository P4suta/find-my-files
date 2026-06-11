//! fmf-core — the entire engine logic of find-my-files.
//!
//! This crate is a pure library: the FFI surface (`fmf-ffi`) and the dev CLI
//! (`fmf-cli`) must not contain logic of their own. See docs/ARCHITECTURE.md
//! for the canonical contract this crate fulfills.

// Declared in dataflow order — reading order = the order data moves
// (ingest: mft/scan → usn → index; search: query → engine; cross-cutting
// last). Names are unchanged; only the narrative order is meaningful.
pub mod mft;
pub mod scan;
pub mod usn;

pub mod index;

pub mod query;

pub mod engine;

pub mod diag;
pub mod metrics;
pub mod wtf8;
