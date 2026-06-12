//! fmf-proto — wire codec for the service pipe. The canonical spec is
//! docs/ARCHITECTURE.md「Pipe プロトコル」; the machine-readable definitions
//! (codes, opcodes, PODs, limits, versions) live in `fmf-contract` and are
//! re-exported here — this crate adds *only* the encode/decode logic, and
//! `tests/golden.rs` pins it byte-for-byte against `contract/golden/`.

pub mod frame;
pub mod messages;

pub use fmf_contract::versions::{
    ABI_VERSION, PIPE_NAME, PIPE_NAME_SHORT, PROTOCOL_VERSION, SERVICE_NAME,
};
pub use fmf_contract::{codes, limits};
