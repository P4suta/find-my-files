//! Status codes — one table shared verbatim by the FFI return values and
//! the pipe frame header (docs/ARCHITECTURE.md エラーコード表).
//!
//! **Append only; renumbering is a breaking protocol change.** Downstream,
//! fmf-ffi's `contract_tests` pin these against literals as an independent
//! tripwire for accidental edits of this file.

/// Success.
pub const OK: i32 = 0;
/// A caller-supplied argument was invalid (null/length contract violated,
/// or a version mismatch on the pipe Hello handshake).
pub const INVALID_ARG: i32 = 1;
/// Structural generation moved (or a result handle was evicted) — the
/// client re-runs the query.
pub const STALE: i32 = 2;
/// The operation needs administrator rights (MFT/USN access) that the
/// caller lacks.
pub const NOT_ADMIN: i32 = 3;
/// A volume could not be opened or read (unsupported filesystem, missing,
/// or otherwise unavailable).
pub const VOLUME: i32 = 4;
/// The query string failed to parse.
pub const QUERY_SYNTAX: i32 = 5;
/// An I/O error occurred (index file or volume read/write).
pub const IO: i32 = 6;
/// The index dir's writer lock is held by another process (single-writer
/// invariant, cross-process).
pub const LOCKED: i32 = 7;
/// An internal panic was caught at the FFI/pipe boundary (`catch_unwind`);
/// detail is available via `fmf_last_error`.
pub const PANIC: i32 = 99;
