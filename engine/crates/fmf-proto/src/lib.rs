//! fmf-proto — wire definitions for the service pipe. The canonical spec is
//! docs/ARCHITECTURE.md「Pipe プロトコル」; this crate is its executable
//! form, shared by fmf-service and tests. fmf-ffi cannot depend on a cdylib
//! and vice versa, so the error codes are duplicated there and pinned equal
//! by fmf-ffi's contract_tests.

pub mod frame;
pub mod messages;

pub const PROTOCOL_VERSION: u32 = 1;

/// Default pipe path. Tests pass their own unique name (`--pipe-name`).
pub const PIPE_NAME: &str = r"\\.\pipe\fmf-engine-v1";

/// Status codes carried in the frame header — the FFI error table verbatim
/// (docs/ARCHITECTURE.md: appending only, renumbering is a breaking change).
pub mod codes {
    pub const OK: i32 = 0;
    pub const INVALID_ARG: i32 = 1;
    pub const STALE: i32 = 2;
    pub const NOT_ADMIN: i32 = 3;
    pub const VOLUME: i32 = 4;
    pub const QUERY_SYNTAX: i32 = 5;
    pub const IO: i32 = 6;
    pub const LOCKED: i32 = 7;
    pub const PANIC: i32 = 99;
}
