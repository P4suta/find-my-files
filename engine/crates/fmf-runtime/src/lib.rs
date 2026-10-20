//! Shared Windows engine runtime for the service, FFI boundary, and developer CLI.
//!
//! Owns operating-system resources, persistence, parallel execution, and shared caches.
//! Deterministic byte decoding and domain rules come from `fmf-core`.
pub mod diag;
#[cfg(windows)]
pub mod engine;
pub mod index;
pub mod metrics;
#[cfg(windows)]
pub mod mft;
pub mod ondisk;
pub mod query;
#[cfg(windows)]
pub mod scan;
pub mod units;
pub mod usn;
#[cfg(windows)]
mod volume_label;
pub mod wtf8;
