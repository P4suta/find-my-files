//! Exact NTFS object and record identities.

const RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// A full 64-bit NTFS file reference number: `(sequence << 48) | record`.
///
/// The identity that survives a rename (the FRN is kept); NTFS reuses the low-48-bit record number, and the sequence distinguishes generations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(transparent)]
pub struct Frn(pub u64);

/// An MFT record number — an [`Frn`]'s low 48 bits.
///
/// The compact index's lookup key.
/// USN mutations add an exact full-[`Frn`] comparison after this lookup so delayed events cannot cross record generations.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[repr(transparent)]
pub struct RecordNo(pub u64);

impl Frn {
    /// The record number this reference points at — its low 48 bits.
    #[inline]
    #[must_use]
    pub const fn record(self) -> RecordNo {
        RecordNo(self.0 & RECORD_MASK)
    }
}

impl From<u64> for RecordNo {
    /// Wrap a value already known to be a record number — the ergonomic entry point for name lookups (the runtime index) and test fixtures.
    /// Derive one from a full reference with [`Frn::record`],
    /// never by truncating a raw `u64` here.
    #[inline]
    fn from(record: u64) -> Self {
        Self(record)
    }
}
