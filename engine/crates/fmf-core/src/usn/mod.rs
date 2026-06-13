//! USN change-journal tailing: pure record parsing (`records`), per-FRN
//! reduction + index application (`apply`), and the OS-facing session
//! (`session`, Windows only).
//!
//! See docs/ARCHITECTURE.md for the threading model and fallback rules.

pub mod apply;
pub mod records;
#[cfg(windows)]
pub mod session;

pub use apply::{BatchStats, NullStatFetcher, StatFetcher, apply_batch};
pub use records::{UsnRecord, parse_buffer, reason};
#[cfg(windows)]
pub use session::{JournalGone, ReadOutcome, UsnError, UsnJournal, VolumeStatFetcher};
