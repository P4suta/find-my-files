//! Journal reasons reduced to index effects.

use super::records::reason;

/// Index operation after complete-link reconciliation has been considered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Remove the exact object generation.
    Delete,
    /// Create or rename a searchable link.
    Upsert,
    /// Refresh shared object metadata.
    Metadata,
    /// No indexed property changed.
    Ignore,
}

/// Accumulated journal reasons for one exact file reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeReasons(u32);

impl ChangeReasons {
    /// Preserve all flags, including reasons the index does not act on.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Accumulate reasons without cancelling a repeated flag.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the record identifies the link before a rename.
    #[must_use]
    pub const fn has_old_name(self) -> bool {
        self.0 & reason::RENAME_OLD_NAME != 0
    }

    /// Whether the object was created during the batch.
    #[must_use]
    pub const fn creates(self) -> bool {
        self.0 & reason::FILE_CREATE != 0
    }

    /// Whether the batch includes a new rename destination.
    #[must_use]
    pub const fn renames(self) -> bool {
        self.0 & reason::RENAME_NEW_NAME != 0
    }

    /// Whether the batch changes the object's hard-link set.
    #[must_use]
    pub const fn changes_hard_links(self) -> bool {
        self.0 & reason::HARD_LINK_CHANGE != 0
    }

    /// Whether a file needs an authoritative link-set lookup.
    #[must_use]
    pub const fn changes_links(self) -> bool {
        self.creates() || self.renames() || self.changes_hard_links()
    }

    /// Reject an incomplete link snapshot for a hard-link change or ambiguous rename.
    #[must_use]
    pub const fn rejects_link_snapshot(self, complete: bool, ambiguous_rename: bool) -> bool {
        !complete && (self.changes_hard_links() || ambiguous_rename)
    }

    /// Classify the remaining mutation with deletion taking precedence.
    #[must_use]
    pub const fn kind(self) -> ChangeKind {
        const METADATA_REASONS: u32 = reason::DATA_OVERWRITE
            + reason::DATA_EXTEND
            + reason::DATA_TRUNCATION
            + reason::BASIC_INFO_CHANGE
            + reason::REPARSE_POINT_CHANGE;

        if self.0 & reason::FILE_DELETE != 0 {
            ChangeKind::Delete
        } else if self.creates() || self.renames() {
            ChangeKind::Upsert
        } else if self.0 & METADATA_REASONS != 0 {
            ChangeKind::Metadata
        } else {
            ChangeKind::Ignore
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_classification_covers_every_indexed_flag_combination() {
        let flags = [
            reason::DATA_OVERWRITE,
            reason::DATA_EXTEND,
            reason::DATA_TRUNCATION,
            reason::BASIC_INFO_CHANGE,
            reason::REPARSE_POINT_CHANGE,
            reason::FILE_CREATE,
            reason::FILE_DELETE,
            reason::RENAME_OLD_NAME,
            reason::RENAME_NEW_NAME,
            reason::HARD_LINK_CHANGE,
        ];
        assert_eq!(
            flags,
            [
                1, 2, 4, 0x8000, 0x10_0000, 0x100, 0x200, 0x1000, 0x2000, 0x1_0000
            ]
        );
        let indexed = flags.iter().fold(0, |bits, flag| bits | flag);
        for subset in 0usize..1 << flags.len() {
            let selected = |index: usize| subset & (1usize << index) != 0;
            let bits =
                flags.iter().enumerate().fold(
                    0,
                    |bits, (index, flag)| {
                        if selected(index) { bits | flag } else { bits }
                    },
                );
            let expected = if selected(6) {
                ChangeKind::Delete
            } else if selected(5) || selected(8) {
                ChangeKind::Upsert
            } else if (0..5).any(selected) {
                ChangeKind::Metadata
            } else {
                ChangeKind::Ignore
            };
            for bits in [bits, bits | !indexed] {
                let reasons = ChangeReasons::from_bits(bits);
                assert_eq!(reasons.kind(), expected, "flags {bits:#x}");
                assert_eq!(reasons.has_old_name(), selected(7));
                assert_eq!(reasons.creates(), selected(5));
                assert_eq!(reasons.renames(), selected(8));
                assert_eq!(reasons.changes_hard_links(), selected(9));
                assert_eq!(
                    reasons.changes_links(),
                    selected(5) || selected(8) || selected(9)
                );
                for complete in [false, true] {
                    for ambiguous in [false, true] {
                        assert_eq!(
                            reasons.rejects_link_snapshot(complete, ambiguous),
                            !complete && (selected(9) || ambiguous)
                        );
                    }
                }
                assert_eq!(reasons.union(reasons), reasons);
                assert_eq!(
                    reasons.union(ChangeReasons::from_bits(!bits)),
                    ChangeReasons::from_bits(u32::MAX)
                );
            }
        }
    }
}

#[cfg(kani)]
#[path = "reason_proofs.rs"]
mod kani_proofs;
