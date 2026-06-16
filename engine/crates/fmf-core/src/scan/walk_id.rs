//! Synthetic record numbers for scope-mode (folder-walk) indexing (ADR-0024).
//!
//! The $MFT scanner gets a real NTFS FRN per record; a folder walk has none.
//! But the index keys identity on [`crate::index::Frn::record`] — the low 48
//! bits, resolved by liveness (`index/frn.rs`), never on the full reference —
//! so any stable, collision-resistant 48-bit key per path serves. We hash the
//! folded absolute path: absolute paths are globally unique, so the low 48
//! bits stay unique across roots without a separate root id (a root id placed
//! in the high bits would be discarded by `record()` anyway), and the Phase 2
//! watcher recomputes the identical key from a changed path with no shared
//! state. Folding the path makes the key case-insensitive, matching NTFS and
//! making the walk's and the watcher's spellings agree.

use xxhash_rust::xxh64::xxh64;

/// The low-48-bit synthetic record number for a folded-WTF-8 absolute path.
///
/// [`crate::index::Frn::record`] masks to 48 bits, so only these bits steer
/// identity; the high 16 bits are left zero. Birthday-bound collisions are
/// ~`n²/2⁴⁹` (well under 0.1% at the scope-mode target of ≤a few hundred
/// thousand entries); a collision merely shadows one path and self-heals on
/// the next walk (ADR-0024).
#[must_use]
pub fn path_record(folded_abs_path: &[u8]) -> u64 {
    xxh64(folded_abs_path, 0) & 0x0000_FFFF_FFFF_FFFF
}
