//! USN change-journal tailing: pure record parsing (`records`), per-FRN
//! reduction + index application (`apply`), and the OS-facing session
//! (`session`, Windows only).
//!
//! Threading: one tail thread per volume, owned by `engine::worker`. It reads
//! non-blocking, drains what is available, and applies it as one batch under
//! the index write lock — a rename storm therefore costs one lock acquisition,
//! not one per record. A quiet journal parks for at most 250ms per iteration
//! and re-checks its stop flag each tick, so shutdown joins promptly without
//! cancelling a blocked I/O.
//!
//! Fallback: recovery from an unusable journal is always a full rescan, never
//! a partial repair. Every case that would require checkpointing past records
//! the index did not absorb — a dead journal id, a truncated FSCTL payload,
//! unavailable hard-link metadata, a rejected record — abandons the batch with
//! its checkpoint unpublished and rebuilds from a fresh journal position.
//! Advancing the cursor past unapplied changes would leave the index silently
//! disagreeing with the volume forever; a rescan is merely slow.

pub mod apply;
pub mod records;
#[cfg(windows)]
pub mod session;

pub use apply::{BatchStats, LinkInfo, MetadataSource, apply_batch};
pub use records::{UsnRecord, parse_buffer, reason};
#[cfg(windows)]
pub use session::{JournalGone, ReadOutcome, UsnError, UsnJournal};
