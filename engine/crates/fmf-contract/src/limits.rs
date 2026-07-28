//! Contractual bounds — protocol facts both sides must agree on, not tunables.
//!
//! A boundary rejects a larger semantic value with
//! [`crate::codes::INVALID_ARG`], and the pipe reader applies the
//! operation-specific cap derived from these *before* it allocates or reads a
//! payload. [`MAX_PAYLOAD_LEN`] bounds response frames and is not a
//! request-allocation allowance.

/// Hard cap on a single pipe frame's payload; announcing more is a protocol
/// violation (connection dropped).
pub const MAX_PAYLOAD_LEN: u32 = 16 * 1024 * 1024;

/// Per-connection result-handle registry cap; beyond it the least recently
/// used handle is evicted (its pages answer STALE with an "evicted" detail).
pub const MAX_RESULTS_PER_CONN: usize = 64;

/// Per-subscriber bounded event queue; overflow drops the oldest event
/// (counted + warned — a slow reader never blocks volume threads).
pub const EVENT_QUEUE_CAP: usize = 256;

/// The client's page-fetch granularity (rows per `ResultPage` request and the
/// UI virtualization page size).
pub const PAGE_ROWS: u32 = 64;

/// Maximum rows accepted by one FFI or pipe page request. Keeping this equal
/// to [`PAGE_ROWS`] bounds row/path materialization before allocation.
pub const MAX_PAGE_ROWS: u32 = PAGE_ROWS;

/// Maximum drive-letter volumes accepted by one indexing request on Windows.
pub const MAX_VOLUMES: u32 = 26;

/// Maximum UTF-8 bytes accepted for one query. This bounds parser/compiler
/// work independently of the much larger response-frame cap.
pub const MAX_QUERY_BYTES: u32 = 4 * 1024;

/// Maximum OR groups in one parsed query. Ordinary interactive queries use
/// one or two; 32 leaves ample power-user headroom while bounding one
/// dictionary sweep/bitset per group.
pub const MAX_QUERY_GROUPS: u32 = 32;

/// Maximum total terms across every OR group.
pub const MAX_QUERY_TERMS: u32 = 128;

/// Maximum `regex:` terms across every OR group. Regex compilation and
/// residual evaluation are materially more expensive than literals.
pub const MAX_REGEX_TERMS: u32 = 8;

/// Maximum canonical JSON bytes accepted for `IndexStart`.
///
/// Twenty-six two-byte drive labels encode well below this; the slack is
/// deliberate, while still preventing a tiny-string JSON array from expanding
/// into an enormous `Vec<String>` before its count is checked.
pub const MAX_INDEX_START_PAYLOAD_LEN: u32 = 512;
