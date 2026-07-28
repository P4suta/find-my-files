//! fmf-proto — wire codec for the service pipe.
//!
//! The definitions (codes, opcodes, PODs, limits, versions) live in
//! `fmf-contract` and are re-exported here; this crate adds *only* the
//! encode/decode logic, and `tests/golden.rs` pins that logic byte-for-byte
//! against the captured corpus in `contract/golden/`, which the C# suite
//! independently decodes and re-encodes. Re-capturing is the explicit
//! `FMF_BLESS=1` ritual, never a side effect of a test run (ADR-0018).
//!
//! The crate is a plain rlib so both the service and the loopback tests link
//! the same codec — nothing here may depend on which side of the pipe it runs
//! on.

pub mod frame;
pub mod messages;

pub use fmf_contract::versions::{
    ABI_VERSION, PIPE_NAME, PIPE_NAME_SHORT, PROTOCOL_VERSION, SERVICE_NAME,
    SERVICE_PROTOCOL_MARKER,
};
pub use fmf_contract::{codes, limits};
