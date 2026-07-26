//! In-memory per-volume index: struct-of-arrays, two string pools with shared
//! offsets, FRN map, and pre-sorted permutations for instant sorting
//! (docs/ARCHITECTURE.md).
//!
//! Mutation model (keeps the permutation arrays merge-only):
//! - create  → append entry + merge into permutations
//! - delete  → tombstone flag only
//! - rename  → files: tombstone the exact old link + append its new path;
//!   dirs: in-place (children point at the `EntryId`), repositioned in `perm_name`
//! - move    → rewrite `parent` only (no permutation depends on the path)
//! - hard link → one live row per directory link; rows share the object's FRN
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
pub(crate) use self::mutate::LinkReconcileStats;

/// Dense, append-only index into the struct-of-arrays entry columns.
///
/// One id identifies one searchable directory link/path. Several ids may
/// share a full [`Frn`] when an NTFS file has hard links.
pub type EntryId = u32;
/// Sentinel `parent` value for an entry attached to the volume root (no parent).
pub const NO_PARENT: EntryId = u32::MAX;

/// Corruption encountered while lazily walking an entry's parent chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathBuildError {
    /// The requested entry id does not identify a row in the index.
    #[error("entry {entry} is outside the index (entries={entries})")]
    EntryOutOfRange {
        /// Invalid entry id supplied by the caller.
        entry: EntryId,
        /// Current index length.
        entries: usize,
    },
    /// An entry or parent id points outside the index columns.
    #[error("entry {entry} references out-of-range parent {parent} (entries={entries})")]
    ParentOutOfRange {
        /// Entry whose parent was read.
        entry: EntryId,
        /// Invalid parent id.
        parent: EntryId,
        /// Current index length.
        entries: usize,
    },
    /// The parent graph contains a cycle and therefore has no root path.
    #[error("parent cycle detected while building the path for entry {entry}")]
    ParentCycle {
        /// Original entry whose path was requested.
        entry: EntryId,
    },
    /// A corrupt parent graph would produce a path larger than any valid
    /// Windows extended-length path.
    #[error(
        "path for entry {entry} exceeds the WTF-8 safety bound ({bytes} bytes > {maximum} bytes)"
    )]
    PathTooLong {
        /// Original entry whose path was requested.
        entry: EntryId,
        /// Bytes required by the path discovered so far.
        bytes: usize,
        /// Maximum possible WTF-8 bytes for a valid NT path.
        maximum: usize,
    },
}

/// Per-entry flag bits packed into the index's `flags` column (one `u8`).
pub mod flags {
    /// This entry is a directory.
    pub const IS_DIR: u8 = 1;
    /// This entry is deleted but not yet compacted away.
    pub const TOMBSTONE: u8 = 2;
    /// This entry is a reparse point (symlink, junction, mount point).
    pub const REPARSE: u8 = 4;
    /// Raw `FILE_ATTRIBUTE_HIDDEN`.
    pub const HIDDEN: u8 = 8;
    /// Raw `FILE_ATTRIBUTE_SYSTEM`.
    pub const SYSTEM: u8 = 16;
    /// Computed: this entry or any ancestor carries HIDDEN|SYSTEM.
    ///
    /// Queries skip these by default (toggleable). Inserts derive it from the
    /// resolved parent; directory moves and H/S changes mark the forest dirty,
    /// then the USN batch boundary propagates inheritance O(n) exactly once.
    pub const EXCLUDED: u8 = 32;
}

/// A full 64-bit NTFS file reference number: `(sequence << 48) | record`.
///
/// The identity that survives a rename (the FRN is kept); NTFS reuses the
/// low-48-bit record number, and the sequence distinguishes generations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(transparent)]
pub struct Frn(pub u64);

/// An MFT record number — an [`Frn`]'s low 48 bits.
///
/// The compact index's lookup key. USN mutations add an exact full-[`Frn`]
/// comparison after this lookup so delayed events cannot cross record
/// generations.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[repr(transparent)]
pub struct RecordNo(pub u64);

impl Frn {
    /// The record number this reference points at — its low 48 bits.
    #[inline]
    #[must_use]
    pub const fn record(self) -> RecordNo {
        RecordNo(self.0 & 0x0000_FFFF_FFFF_FFFF)
    }
}

impl From<u64> for RecordNo {
    /// Wrap a value already known to be a record number — the ergonomic
    /// entry point for name lookups ([`VolumeIndex::entry_by_record`]) and
    /// test fixtures. Derive one from a full reference with [`Frn::record`],
    /// never by truncating a raw `u64` here.
    #[inline]
    fn from(record: u64) -> Self {
        Self(record)
    }
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

/// One record produced by an initial-scan source (raw $MFT today, `ReFS`
/// enumeration in the future).
pub struct RawEntry<'a> {
    /// The parent directory's full reference. The initial builder resolves its
    /// record after the scan; incremental USN insertion requires an exact
    /// sequence match. An unknown parent attaches the entry to the root.
    pub parent_frn: Frn,
    /// The underlying object's full reference. Hard-linked entries share it;
    /// link identity additionally includes `parent_frn` + `name_utf16`.
    pub frn: Frn,
    /// The file name as raw UTF-16 code units (as stored in the MFT).
    pub name_utf16: &'a [u16],
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Whether this entry is a reparse point (symlink, junction, mount point).
    pub is_reparse: bool,
    /// Raw `FILE_ATTRIBUTE_HIDDEN`.
    pub is_hidden: bool,
    /// Raw `FILE_ATTRIBUTE_SYSTEM`.
    pub is_system: bool,
    /// File size in bytes.
    pub size: u64,
    /// FILETIME (100ns ticks since 1601, UTC).
    pub mtime: i64,
}

/// A [`RawEntry`] whose name is already WTF-8 encoded.
///
/// Parallel scan workers encode off the builder thread, the builder just
/// memcpys. `name_wtf8`/`lower_wtf8` must come from
/// [`crate::wtf8::push_wtf8_pair`] (equal lengths, shared offsets).
pub struct EncodedEntry<'a> {
    /// The parent directory's full reference; initial-scan finish resolves it
    /// after all records are present.
    pub parent_frn: Frn,
    /// The underlying object's complete record+sequence reference.
    pub frn: Frn,
    /// The file name, WTF-8 encoded (paired with `lower_wtf8`, shared offsets).
    pub name_wtf8: &'a [u8],
    /// The lowercased file name, WTF-8 encoded (same length as `name_wtf8`).
    pub lower_wtf8: &'a [u8],
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Whether this entry is a reparse point (symlink, junction, mount point).
    pub is_reparse: bool,
    /// Raw `FILE_ATTRIBUTE_HIDDEN`.
    pub is_hidden: bool,
    /// Raw `FILE_ATTRIBUTE_SYSTEM`.
    pub is_system: bool,
    /// File size in bytes.
    pub size: u64,
    /// FILETIME (100ns ticks since 1601, UTC).
    pub mtime: i64,
}

// The sort key is contract surface (FmfQueryOptions.sort carries it as
// u32) — the index uses the canonical definition directly (ADR-0018).
pub use fmf_contract::options::SortKey;
