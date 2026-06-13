//! Status codes — one table shared verbatim by the FFI return values and
//! the pipe frame header (docs/ARCHITECTURE.md エラーコード表).
//!
//! **Append only; renumbering is a breaking protocol change.** Downstream,
//! fmf-ffi's `contract_tests` pin these against literals as an independent
//! tripwire for accidental edits of this file.

pub const OK: i32 = 0;
pub const INVALID_ARG: i32 = 1;
/// Structural generation moved (or a result handle was evicted) — the
/// client re-runs the query.
pub const STALE: i32 = 2;
pub const NOT_ADMIN: i32 = 3;
pub const VOLUME: i32 = 4;
pub const QUERY_SYNTAX: i32 = 5;
pub const IO: i32 = 6;
/// The index dir's writer lock is held by another process (single-writer
/// invariant, cross-process).
pub const LOCKED: i32 = 7;
pub const PANIC: i32 = 99;
