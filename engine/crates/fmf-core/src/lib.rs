//! Deterministic filename-index rules and byte decoders.
//!
//! Operating-system access, persistence, shared caches, and execution belong to `fmf-runtime`.
#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::expect_used, clippy::unwrap_used)
)]

#[cfg_attr(test, macro_use)]
extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod dates;
pub mod histogram;
pub mod identity;
pub mod ondisk;
pub mod recovery;
pub mod usn;
pub mod wtf8;
