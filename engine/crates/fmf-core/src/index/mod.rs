//! In-memory per-volume index: struct-of-arrays, two string pools with shared
//! offsets, FRN map, and pre-sorted permutations for instant sorting
//! (docs/ARCHITECTURE.md).
//!
//! Mutation model (keeps the permutation arrays merge-only):
//! - create  → append entry + merge into permutations
//! - delete  → tombstone flag only
//! - rename  → files: tombstone old + append new entry (same FRN);
//!   dirs: in-place (children point at the EntryId), repositioned in perm_name
//! - move    → rewrite `parent` only (no permutation depends on the path)
//!
//! Tombstones accumulate until compaction (M2), which bumps
//! `structural_generation` and invalidates open result handles. Ordinary
//! batches bump `content_generation` only; open results stay readable.

mod builder;
mod compact;
mod core;
mod frn;
mod mutate;
mod snapshot;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use self::builder::{FinishTimings, VolumeIndexBuilder};
pub use self::core::VolumeIndex;

pub type EntryId = u32;
pub const NO_PARENT: EntryId = u32::MAX;

pub mod flags {
    pub const IS_DIR: u8 = 1;
    pub const TOMBSTONE: u8 = 2;
    pub const REPARSE: u8 = 4;
    /// Raw FILE_ATTRIBUTE_HIDDEN.
    pub const HIDDEN: u8 = 8;
    /// Raw FILE_ATTRIBUTE_SYSTEM.
    pub const SYSTEM: u8 = 16;
    /// Computed: this entry or any ancestor carries HIDDEN|SYSTEM. Queries
    /// skip these by default (toggleable). Kept in sync on insert/move; a
    /// subtree moved out of an excluded branch keeps stale bits until the
    /// next full rescan (same accepted-limitation class as dir renames).
    pub const EXCLUDED: u8 = 32;
}

/// Mask an NTFS file reference number down to the record number (low 48 bits).
#[inline]
pub fn masked(record_or_frn: u64) -> u64 {
    record_or_frn & 0x0000_FFFF_FFFF_FFFF
}

/// `reserve_exact` with a `len/64` floor, for merge-only arrays that grow a
/// small batch at a time: bounds both copy frequency and permanent slack to
/// ~1.6% (doubling would pin up to 2× as slack against the B/entry RAM gate).
fn reserve_bounded<T>(v: &mut Vec<T>, additional: usize) {
    let want = v.len() + additional;
    if want > v.capacity() {
        v.reserve_exact(additional.max(v.len() / 64));
    }
}

/// Merge sorted `batch` into `perm` in place: each batch element
/// binary-searches its insertion point and the segments between insertion
/// points move once with `copy_within` — O(batch·log n) comparisons +
/// O(moved) memmove + no allocation (ADR-0008).
///
/// Old elements are never reordered, and on a sorted array the strict
/// total order (`cmp` id tie-break) makes the result the unique sorted
/// merge. Arrays ordered by size/mtime can be locally stale-sorted
/// (in-place `update_stat` never repositions an entry); placement there is
/// deterministic best-effort.
pub(crate) fn merge_sorted_tail(
    perm: &mut Vec<EntryId>,
    batch: &[EntryId],
    cmp: impl Fn(EntryId, EntryId) -> std::cmp::Ordering,
) {
    if batch.is_empty() {
        return;
    }
    let old = perm.len();
    reserve_bounded(perm, batch.len());
    perm.resize(old + batch.len(), 0);
    let mut hi = old; // unmoved prefix of the old array (exclusive end)
    let mut k = old + batch.len(); // write cursor (exclusive end)
    for j in (0..batch.len()).rev() {
        let b = batch[j];
        // First old index whose element orders after `b`.
        let pos = perm[..hi].partition_point(|&x| !cmp(x, b).is_gt());
        let seg = hi - pos;
        perm.copy_within(pos..hi, k - seg);
        k -= seg + 1;
        perm[k] = b;
        hi = pos;
    }
    // k - hi == unplaced batch elements throughout; both cursors meet here.
    debug_assert_eq!(k, hi, "merge cursors must close");
}

/// One record produced by an initial-scan source (raw $MFT today, ReFS
/// enumeration in the future).
pub struct RawEntry<'a> {
    pub record: u64,
    pub parent_record: u64,
    /// Full FRN including the sequence value.
    pub frn: u64,
    pub name_utf16: &'a [u16],
    pub is_dir: bool,
    pub is_reparse: bool,
    pub is_hidden: bool,
    pub is_system: bool,
    pub size: u64,
    /// FILETIME (100ns ticks since 1601, UTC).
    pub mtime: i64,
}

/// A [`RawEntry`] whose name is already WTF-8 encoded — parallel scan
/// workers encode off the builder thread, the builder just memcpys.
/// `name_wtf8`/`lower_wtf8` must come from [`crate::wtf8::push_wtf8_pair`]
/// (equal lengths, shared offsets).
pub struct EncodedEntry<'a> {
    pub record: u64,
    pub parent_record: u64,
    pub frn: u64,
    pub name_wtf8: &'a [u8],
    pub lower_wtf8: &'a [u8],
    pub is_dir: bool,
    pub is_reparse: bool,
    pub is_hidden: bool,
    pub is_system: bool,
    pub size: u64,
    pub mtime: i64,
}

// The sort key is contract surface (FmfQueryOptions.sort carries it as
// u32) — the index uses the canonical definition directly (ADR-0018).
pub use fmf_contract::options::SortKey;
