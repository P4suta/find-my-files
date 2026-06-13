//! Contractual bounds (docs/ARCHITECTURE.md). These are protocol facts both
//! sides must agree on, not tunables.

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
/// UI virtualization page size). The wire itself accepts any count.
pub const PAGE_ROWS: u32 = 64;
