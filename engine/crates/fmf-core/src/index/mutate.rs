use crate::wtf8;

use super::core::SortColumns;
use super::{
    EncodedEntry, EntryId, Frn, NO_PARENT, RawEntry, RecordNo, SortKey, VolumeIndex, flags,
    merge_sorted_tail,
};

/// Changes made while converging one NTFS object's searchable file-link set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkReconcileStats {
    pub added: u32,
    pub removed: u32,
    pub retained: u32,
    /// At least one retained row's object-owned metadata differed.
    pub metadata_changed: bool,
}

/// A live NTFS mutation could not preserve an exact rooted topology.
///
/// Callers must reject the complete USN batch, avoid checkpointing it, and
/// request a clean volume rescan. None of these conditions is recoverable by
/// guessing the root directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IndexMutationError {
    /// A production event carried a sequence-zero object or parent reference.
    #[error("USN mutation carries sequence-zero reference {reference:?}")]
    InvalidReference {
        /// Invalid full reference.
        reference: Frn,
    },
    /// No live row matched the exact parent generation.
    #[error("object {object:?} references missing/stale parent {parent:?}")]
    UnresolvedParent {
        /// Object being inserted or moved.
        object: Frn,
        /// Missing or stale parent generation.
        parent: Frn,
    },
    /// The exact referenced object exists but is not a directory.
    #[error("object {object:?} references non-directory parent {parent:?}")]
    ParentNotDirectory {
        /// Object being inserted or moved.
        object: Frn,
        /// Exact non-directory parent reference.
        parent: Frn,
    },
    /// More than one live directory row matched one exact parent generation.
    #[error("object {object:?} has ambiguous exact parent {parent:?}")]
    AmbiguousParent {
        /// Object being inserted or moved.
        object: Frn,
        /// Ambiguous parent reference.
        parent: Frn,
    },
    /// An object named itself as parent.
    #[error("object {object:?} names itself as parent")]
    SelfParent {
        /// Self-parented object.
        object: Frn,
    },
    /// Applying a directory move would create a parent cycle.
    #[error("moving object {object:?} below {parent:?} would create a parent cycle")]
    ParentCycle {
        /// Directory being moved.
        object: Frn,
        /// Proposed parent directory.
        parent: Frn,
    },
    /// Existing live topology is already corrupt or incomplete.
    #[error("live index topology is invalid: {reason}")]
    InvalidTopology {
        /// Stable diagnostic reason.
        reason: &'static str,
    },
    /// An authoritative link snapshot was empty.
    #[error("authoritative link snapshot for {object:?} is empty")]
    EmptyLinkSnapshot {
        /// File object whose snapshot was empty.
        object: Frn,
    },
    /// An authoritative link snapshot repeated one exact path identity.
    #[error("authoritative link snapshot for {object:?} contains a duplicate path")]
    DuplicateLink {
        /// File object whose snapshot was not a set.
        object: Frn,
    },
    /// Rows in one authoritative link snapshot disagreed on object metadata.
    #[error("authoritative link snapshot for {object:?} has inconsistent object metadata")]
    InconsistentLinkMetadata {
        /// File object whose rows disagreed.
        object: Frn,
    },
    /// A live mutation supplied an empty filename.
    #[error("USN mutation for {object:?} supplied an empty filename")]
    EmptyName {
        /// Object carrying the empty name.
        object: Frn,
    },
    /// A directory-only operation targeted a file, or vice versa.
    #[error("USN mutation for {object:?} has an incompatible object kind")]
    ObjectKind {
        /// Object carrying the incompatible kind.
        object: Frn,
    },
}

impl VolumeIndex {
    // ── Incremental mutation (USN batches; see module docs) ──────────────

    /// Pool bytes `id` *owns* that a rebuild reclaims: its original-spelling
    /// copy. The folded bytes are shared in the dictionary (ADR-0032), so a
    /// dead entry rarely frees them; their bloat is tracked by
    /// `dict_appends_since_dedup` and collapsed at the next dedup instead.
    fn owned_name_bytes(&self, id: EntryId) -> u64 {
        if self.is_fold_identical(id) {
            0
        } else {
            self.name_len_of(id) as u64
        }
    }

    pub(super) fn tombstone_id(&mut self, id: EntryId) {
        debug_assert!(self.is_live(id), "a link row is tombstoned once");
        self.flag[id as usize] |= flags::TOMBSTONE;
        self.tombstones += 1;
        self.dead_name_bytes += self.owned_name_bytes(id);
    }

    fn tombstone_record(&mut self, record: RecordNo) -> Option<EntryId> {
        let ids: Vec<_> = self.entries_by_record(record).collect();
        let first = ids.first().copied();
        for id in ids {
            self.tombstone_id(id);
        }
        first
    }

    fn tombstone_frn(&mut self, frn: Frn) -> Option<EntryId> {
        let ids: Vec<_> = self.entries_by_frn(frn).collect();
        let first = ids.first().copied();
        for id in ids {
            self.tombstone_id(id);
        }
        first
    }

    fn validate_mutation_reference(
        &self,
        reference: Frn,
        synthetic_root_ok: bool,
    ) -> Result<(), IndexMutationError> {
        if reference.0 >> 48 != 0
            || (synthetic_root_ok
                && self.is_synthetic_fixture()
                && reference.record() == self.frn(Self::ROOT).record())
        {
            Ok(())
        } else {
            Err(IndexMutationError::InvalidReference { reference })
        }
    }

    fn ensure_parent_does_not_cycle(
        &self,
        object: Frn,
        parent: EntryId,
    ) -> Result<(), IndexMutationError> {
        let parent_frn = self.frn(parent);
        if object.record() == parent_frn.record() {
            return Err(IndexMutationError::SelfParent { object });
        }
        let mut object_dirs = self.entries_by_frn(object).filter(|&id| self.is_dir(id));
        let object_dir = object_dirs.next();
        if object_dirs.next().is_some() {
            return Err(IndexMutationError::InvalidTopology {
                reason: "one directory object has multiple live rows",
            });
        }
        let Some(object_dir) = object_dir else {
            return Ok(());
        };

        let mut current = parent;
        for _ in 0..=self.len() {
            if current == object_dir {
                return Err(IndexMutationError::ParentCycle {
                    object,
                    parent: parent_frn,
                });
            }
            if current == Self::ROOT {
                return Ok(());
            }
            if current == NO_PARENT || current as usize >= self.len() || !self.is_live(current) {
                return Err(IndexMutationError::InvalidTopology {
                    reason: "a live parent chain escapes the live index",
                });
            }
            if !self.is_dir(current) {
                return Err(IndexMutationError::InvalidTopology {
                    reason: "a parent chain traverses a non-directory row",
                });
            }
            current = self.parent(current);
        }
        Err(IndexMutationError::InvalidTopology {
            reason: "the existing live parent graph contains a cycle",
        })
    }

    fn resolve_parent_for_mutation(
        &self,
        object: Frn,
        parent_reference: Frn,
    ) -> Result<EntryId, IndexMutationError> {
        self.validate_mutation_reference(object, false)?;
        self.validate_mutation_reference(parent_reference, true)?;
        if object.record() == parent_reference.record() {
            return Err(IndexMutationError::SelfParent { object });
        }

        let mut directory = None;
        let mut exact_non_directory = false;
        for id in self.entries_by_frn(parent_reference) {
            if self.is_dir(id) {
                if directory.replace(id).is_some() {
                    return Err(IndexMutationError::AmbiguousParent {
                        object,
                        parent: parent_reference,
                    });
                }
            } else {
                exact_non_directory = true;
            }
        }
        let parent = if let Some(parent) = directory {
            parent
        } else if self.is_synthetic_fixture()
            && self.frn(Self::ROOT).record() == parent_reference.record()
        {
            Self::ROOT
        } else if exact_non_directory {
            return Err(IndexMutationError::ParentNotDirectory {
                object,
                parent: parent_reference,
            });
        } else {
            return Err(IndexMutationError::UnresolvedParent {
                object,
                parent: parent_reference,
            });
        };
        self.ensure_parent_does_not_cycle(object, parent)?;
        Ok(parent)
    }

    fn validate_existing_object_kind(
        &self,
        object: Frn,
        is_dir: bool,
    ) -> Result<usize, IndexMutationError> {
        let ids: Vec<_> = self.entries_by_frn(object).collect();
        if ids.iter().any(|&id| self.is_dir(id) != is_dir) {
            return Err(IndexMutationError::ObjectKind { object });
        }
        if is_dir && ids.len() > 1 {
            return Err(IndexMutationError::InvalidTopology {
                reason: "one directory object has multiple live rows",
            });
        }
        Ok(ids.len())
    }

    fn validate_live_topology(&self) -> Result<(), IndexMutationError> {
        // Parent-chain walk marks: on the stack now / proven rooted already.
        const VISITING: u8 = 1;
        const DONE: u8 = 2;

        if self.is_empty()
            || !self.is_live(Self::ROOT)
            || !self.is_dir(Self::ROOT)
            || self.parent(Self::ROOT) != NO_PARENT
        {
            return Err(IndexMutationError::InvalidTopology {
                reason: "the root is missing, dead, non-directory, or parented",
            });
        }
        if !self
            .frn_index
            .has_valid_live_object_groups(&self.frn, &self.flag)
        {
            return Err(IndexMutationError::InvalidTopology {
                reason: "record generations or object kinds are ambiguous",
            });
        }
        if !self.has_unique_live_link_identities() {
            return Err(IndexMutationError::InvalidTopology {
                reason: "an exact hard-link identity occurs more than once",
            });
        }

        for entry in 1..self.len() as EntryId {
            if !self.is_live(entry) {
                continue;
            }
            self.validate_mutation_reference(self.frn(entry), false)?;
            let parent = self.parent(entry);
            if parent == NO_PARENT
                || parent as usize >= self.len()
                || !self.is_live(parent)
                || !self.is_dir(parent)
            {
                return Err(IndexMutationError::InvalidTopology {
                    reason: "a live entry lacks one live directory parent",
                });
            }
            if self.frn(entry).record() == self.frn(parent).record() {
                return Err(IndexMutationError::SelfParent {
                    object: self.frn(entry),
                });
            }
        }

        let mut state = vec![0u8; self.len()];
        state[Self::ROOT as usize] = DONE;
        let mut stack = Vec::new();
        for start in 1..self.len() as EntryId {
            if !self.is_live(start) || state[start as usize] == DONE {
                continue;
            }
            stack.clear();
            let mut current = start;
            while current != Self::ROOT && state[current as usize] != DONE {
                if state[current as usize] == VISITING {
                    return Err(IndexMutationError::InvalidTopology {
                        reason: "the live parent graph contains a cycle",
                    });
                }
                state[current as usize] = VISITING;
                stack.push(current);
                current = self.parent(current);
            }
            while let Some(entry) = stack.pop() {
                state[entry as usize] = DONE;
            }
        }
        Ok(())
    }

    /// Insert or replace an NTFS object for `record`. Replacement tombstones
    /// every directory-link row for that record, then appends one new row.
    ///
    /// This object-level compatibility API is useful for tests and synthetic
    /// sources. Live USN creation uses exact link-level insertion internally so
    /// the object's other hard-linked paths remain visible.
    /// Returns the new id. Caller must finish the batch with
    /// `merge_new_into_permutations` (crate-internal, so it cannot be linked
    /// from this public item).
    pub fn upsert_synthetic(&mut self, e: &RawEntry) -> EntryId {
        self.tombstone_record(e.frn.record());
        // Synthetic fixtures intentionally retain the old orphan convenience.
        // Production USN paths use the exact, fallible methods below.
        let parent = self
            .entry_by_record(e.parent_frn.record())
            .unwrap_or(Self::ROOT);
        self.push_raw(e, parent)
    }

    /// USN object replacement retained for operations known to represent the
    /// one directory row (notably directory create/rename). File-link events
    /// use [`Self::upsert_link_usn`].
    pub(crate) fn upsert_usn(&mut self, e: &RawEntry) -> Result<EntryId, IndexMutationError> {
        if e.name_utf16.is_empty() {
            return Err(IndexMutationError::EmptyName { object: e.frn });
        }
        let parent = self.resolve_parent_for_mutation(e.frn, e.parent_frn)?;
        let existing = self.validate_existing_object_kind(e.frn, e.is_dir)?;
        if !e.is_dir && existing > 1 {
            return Err(IndexMutationError::InvalidTopology {
                reason: "object-level replacement would discard sibling hard links",
            });
        }
        self.tombstone_record(e.frn.record());
        Ok(self.push_raw(e, parent))
    }

    /// Insert or refresh one exact file-link row without deleting the object's
    /// other paths. A reused MFT record first tombstones every row belonging to
    /// the old sequence; an exact duplicate link is replaced rather than
    /// emitted twice.
    pub(crate) fn upsert_link_usn(&mut self, e: &RawEntry) -> Result<EntryId, IndexMutationError> {
        if e.is_dir {
            return Err(IndexMutationError::ObjectKind { object: e.frn });
        }
        if e.name_utf16.is_empty() {
            return Err(IndexMutationError::EmptyName { object: e.frn });
        }
        let parent = self.resolve_parent_for_mutation(e.frn, e.parent_frn)?;
        self.validate_existing_object_kind(e.frn, false)?;
        let stale: Vec<_> = self
            .entries_by_record(e.frn.record())
            .filter(|&id| self.frn(id) != e.frn)
            .collect();
        for id in stale {
            self.tombstone_id(id);
        }

        // Bound convergence independently of `tombstone_id`'s side effect so
        // a broken tombstone operation cannot turn a corrupt duplicate set
        // into an infinite loop.
        let candidate_count = self.entries_by_frn(e.frn).count();
        for _ in 0..candidate_count {
            let Some(old) = self.entry_by_link(e.frn, e.parent_frn, e.name_utf16) else {
                break;
            };
            self.tombstone_id(old);
        }
        Ok(self.push_raw(e, parent))
    }

    /// Converge one file object's rows to a complete, authoritative link set.
    ///
    /// The metadata source promises a non-empty, duplicate-free set; this
    /// method independently verifies that promise before mutating anything,
    /// preserves matching `EntryId`s, tombstones disappeared links, appends new
    /// links, and retires every row from an older generation if the MFT record
    /// was reused.
    pub(crate) fn reconcile_file_links_usn(
        &mut self,
        frn: Frn,
        desired: &[RawEntry<'_>],
    ) -> Result<LinkReconcileStats, IndexMutationError> {
        if desired.is_empty() {
            return Err(IndexMutationError::EmptyLinkSnapshot { object: frn });
        }
        self.validate_mutation_reference(frn, false)?;
        let metadata = &desired[0];
        if metadata.frn != frn || metadata.is_dir {
            return Err(IndexMutationError::ObjectKind { object: frn });
        }
        self.validate_existing_object_kind(frn, false)?;

        // Resolve and validate the complete authoritative set before mutating
        // any row, so a stale parent or duplicate cannot commit a valid prefix.
        let mut wanted: Vec<(EntryId, Vec<u8>, &RawEntry<'_>)> = Vec::with_capacity(desired.len());
        for entry in desired {
            if entry.frn != frn || entry.is_dir {
                return Err(IndexMutationError::ObjectKind { object: frn });
            }
            if entry.name_utf16.is_empty() {
                return Err(IndexMutationError::EmptyName { object: frn });
            }
            if entry.size != metadata.size
                || entry.mtime != metadata.mtime
                || entry.is_reparse != metadata.is_reparse
                || entry.is_hidden != metadata.is_hidden
                || entry.is_system != metadata.is_system
            {
                return Err(IndexMutationError::InconsistentLinkMetadata { object: frn });
            }
            let parent = self.resolve_parent_for_mutation(entry.frn, entry.parent_frn)?;
            let mut name = Vec::with_capacity(entry.name_utf16.len() * 3);
            let mut folded = Vec::with_capacity(entry.name_utf16.len() * 3);
            wtf8::push_wtf8_pair(entry.name_utf16, &mut name, &mut folded);
            if wanted
                .iter()
                .any(|(p, n, _)| *p == parent && n.as_slice() == name.as_slice())
            {
                return Err(IndexMutationError::DuplicateLink { object: frn });
            }
            wanted.push((parent, name, entry));
        }

        let stale: Vec<_> = self
            .entries_by_record(frn.record())
            .filter(|&id| self.frn(id) != frn)
            .collect();
        let mut stats = LinkReconcileStats {
            removed: stale.len() as u32,
            ..LinkReconcileStats::default()
        };
        for id in stale {
            self.tombstone_id(id);
        }

        let existing: Vec<_> = self.entries_by_frn(frn).collect();
        let mut matched = vec![false; wanted.len()];
        for id in existing {
            let found = wanted
                .iter()
                .enumerate()
                .position(|(i, (parent, name, _))| {
                    !matched[i] && self.parent(id) == *parent && self.name(id) == name.as_slice()
                });
            if let Some(i) = found {
                matched[i] = true;
                stats.retained += 1;
            } else {
                self.tombstone_id(id);
                stats.removed += 1;
            }
        }

        for (i, (parent, _, entry)) in wanted.iter().enumerate() {
            if !matched[i] {
                self.push_raw(entry, *parent);
                stats.added += 1;
            }
        }

        // Size/mtime and STANDARD_INFORMATION attributes belong to the object,
        // so refresh every retained/appended path from the same snapshot.
        let metadata = wanted[0].2;
        let expected_mtime = crate::query::dates::mtime_ticks_to_secs(metadata.mtime);
        stats.metadata_changed = self
            .entries_by_frn(frn)
            .any(|id| self.size(id) != metadata.size || self.mtime[id as usize] != expected_mtime);
        self.update_stat_frn(frn, metadata.size, metadata.mtime);
        stats.metadata_changed |= self
            .update_object_attrs_frn(
                frn,
                metadata.is_reparse,
                metadata.is_hidden,
                metadata.is_system,
            )
            .unwrap_or(false);
        Ok(stats)
    }

    /// Tombstone every link row for a record number. The FRN index never finds
    /// dead entries (liveness filter), so there is nothing to unmap.
    pub fn delete(&mut self, record: impl Into<RecordNo>) -> Option<EntryId> {
        self.tombstone_record(record.into())
    }

    /// Tombstone every link of the exact NTFS object generation. A final
    /// `FILE_DELETE` removes the object; removing just one hard link is handled
    /// by [`Self::delete_link_frn`].
    pub(crate) fn delete_frn(&mut self, frn: Frn) -> Option<EntryId> {
        self.tombstone_frn(frn)
    }

    /// Tombstone one exact hard-link identity. Delayed events for an older
    /// object sequence or an already-removed path are no-ops.
    pub(crate) fn delete_link_frn(
        &mut self,
        frn: Frn,
        parent_frn: Frn,
        name_utf16: &[u16],
    ) -> Option<EntryId> {
        let id = self.entry_by_link(frn, parent_frn, name_utf16)?;
        self.tombstone_id(id);
        Some(id)
    }

    /// Synthetic-only move by bare record number. Unknown and self parents
    /// retain fixture conveniences; production uses exact fallible mutations.
    pub fn reparent_synthetic(
        &mut self,
        record: impl Into<RecordNo>,
        new_parent_record: impl Into<RecordNo>,
    ) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let parent = self
            .entry_by_record(new_parent_record)
            .unwrap_or(Self::ROOT);
        let parent_changed = parent != id && self.parent[id as usize] != parent;
        if parent != id {
            self.parent[id as usize] = parent;
        }
        self.recompute_excluded(id);
        if self.is_dir(id) {
            self.dir_topology_generation += 1; // descendant paths moved
            self.exclusion_tree_dirty |= parent_changed;
        }
        Some(id)
    }

    /// Synthetic-only rename/move of a *directory* in place. Directories keep their
    /// `EntryId` stable — children's `parent` fields point at it — so instead
    /// of tombstone+new (the file path), the name is swapped and the entry is
    /// repositioned inside `perm_name`. O(len) per rename; directory renames
    /// are rare enough that this beats invalidating every child.
    pub fn rename_dir_synthetic_in_place(
        &mut self,
        record: impl Into<RecordNo>,
        name_utf16: &[u16],
        new_parent_record: impl Into<RecordNo>,
    ) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let parent = self
            .entry_by_record(new_parent_record)
            .unwrap_or(Self::ROOT);
        self.rename_dir_id_in_place(id, name_utf16, parent)
    }

    /// Rename/move a directory only when the complete object identity still
    /// matches. Both the object and its new parent are sequence-checked.
    pub(crate) fn rename_dir_frn_in_place(
        &mut self,
        frn: Frn,
        name_utf16: &[u16],
        new_parent_frn: Frn,
    ) -> Result<Option<EntryId>, IndexMutationError> {
        self.validate_mutation_reference(frn, false)?;
        if name_utf16.is_empty() {
            return Err(IndexMutationError::EmptyName { object: frn });
        }
        if self.validate_existing_object_kind(frn, true)? == 0 {
            return Ok(None);
        }
        let Some(id) = self.entry_by_frn(frn) else {
            return Err(IndexMutationError::InvalidTopology {
                reason: "validated directory identity disappeared during mutation",
            });
        };
        let parent = self.resolve_parent_for_mutation(frn, new_parent_frn)?;
        Ok(self.rename_dir_id_in_place(id, name_utf16, parent))
    }

    fn rename_dir_id_in_place(
        &mut self,
        id: EntryId,
        name_utf16: &[u16],
        parent: EntryId,
    ) -> Option<EntryId> {
        let pos = self.perm_name.iter().position(|&x| x == id)?;
        self.perm_name.remove(pos);

        // The old name's original copy is abandoned; its folded dict entry
        // becomes unreferenced (reclaimed at the next dedup, ADR-0032).
        self.dead_name_bytes += self.owned_name_bytes(id);
        let off = self.dict_pool.len();
        let mut orig = Vec::with_capacity(name_utf16.len() * 3);
        wtf8::push_wtf8_pair(name_utf16, &mut orig, &mut self.dict_pool);
        self.name_id[id as usize] = self.push_dict_entry(off);
        self.orig_off[id as usize] = self.push_orig_if_differs(off, &orig);
        let parent_changed = parent != id && self.parent[id as usize] != parent;
        if parent != id {
            self.parent[id as usize] = parent;
        }
        self.recompute_excluded(id);

        let ins = self
            .perm_name
            .binary_search_by(|&x| self.cmp_by(SortKey::Name, x, id))
            .unwrap_or_else(|e| e);
        self.perm_name.insert(ins, id);
        self.dir_topology_generation += 1; // descendant paths renamed
        self.exclusion_tree_dirty |= parent_changed;
        Some(id)
    }

    /// Update size/mtime in place for every link of the object.
    pub fn update_stat(
        &mut self,
        record: impl Into<RecordNo>,
        size: u64,
        mtime: i64,
    ) -> Option<EntryId> {
        let ids: Vec<_> = self.entries_by_record(record).collect();
        let first = ids.first().copied();
        let mut changed = false;
        for id in ids {
            changed |= self.update_stat_id(id, size, mtime);
        }
        if changed {
            self.stat_generation += 1;
        }
        first
    }

    /// Update metadata on every link of the exact NTFS object generation.
    pub(crate) fn update_stat_frn(&mut self, frn: Frn, size: u64, mtime: i64) -> Option<EntryId> {
        let ids: Vec<_> = self.entries_by_frn(frn).collect();
        let first = ids.first().copied();
        let mut changed = false;
        for id in ids {
            changed |= self.update_stat_id(id, size, mtime);
        }
        if changed {
            self.stat_generation += 1;
        }
        first
    }

    fn update_stat_id(&mut self, id: EntryId, size: u64, mtime: i64) -> bool {
        let mtime = crate::query::dates::mtime_ticks_to_secs(mtime);
        if self.size(id) == size && self.mtime[id as usize] == mtime {
            return false;
        }
        self.set_size(id, size);
        self.mtime[id as usize] = mtime;
        true
    }

    /// Merge entries `first_new..len` (already appended, unsorted) into the
    /// name permutation (in place — see `merge_sorted_tail`), then bump
    /// the content generation. Call once per USN batch. Lazy size/mtime
    /// permutations extend for append-only batches; stat mutations carry a
    /// separate generation and force an exact lazy rebuild.
    ///
    /// # Errors
    ///
    /// Returns [`IndexMutationError`] when a production index no longer forms
    /// one exact rooted forest. The caller must discard the batch and rescan.
    /// The rejection is *reported*, never half-applied: see below.
    pub(crate) fn merge_new_into_permutations(
        &mut self,
        first_new: EntryId,
    ) -> Result<(), IndexMutationError> {
        // The FRN index rides the same batch boundary (its own watermark).
        {
            let Self {
                frn_index,
                frn,
                flag,
                ..
            } = self;
            frn_index.merge_appended(frn, flag);
        }
        // Topology validation is read-only, so its verdict is taken here and
        // returned at the end rather than short-circuiting the merge. Bailing
        // out early would leave the index describing something no code path
        // can read safely: `perm_name` shorter than the entry columns (name
        // order silently omits the appended rows) while the derived caches
        // keep answering from the unchanged content generation (a size/mtime
        // order does include them, so a `path:` query then indexes a topology
        // built for fewer entries). `snapshot.rs` refuses to load exactly that
        // shape — the live index must not be allowed to hold it either, least
        // of all for the tens of seconds a rescan takes. So the index is
        // always left self-consistent and the batch is rejected through the
        // return value: `usn::apply` turns it into `index_rejections +
        // rescan_required`, which forbids checkpointing and discards these
        // rows by rebuilding, not by tearing the index in place.
        let verdict = if self.is_synthetic_fixture() {
            Ok(())
        } else {
            self.validate_live_topology()
        };
        if self.exclusion_tree_dirty {
            self.recompute_all_excluded();
            self.exclusion_tree_dirty = false;
        }
        let mut batch: Vec<EntryId> = (first_new..self.len() as u32).collect();
        if !batch.is_empty() {
            // Decorate-sort (ADR-0033): the batch sort does O(B log B)
            // comparisons, each of which would resolve two folded names
            // through the `name_id → dict` indirection (ADR-0032). Resolve
            // each name once, up front, and sort on the borrowed slices —
            // byte-identical order (name then id, like `cmp_by(Name)`), far
            // fewer dict derefs. The merge below still resolves the existing
            // `perm_name` side through `cols`.
            {
                let mut decorated: Vec<(&[u8], EntryId)> =
                    batch.iter().map(|&id| (self.lower_name(id), id)).collect();
                decorated.sort_unstable_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.cmp(&b.1)));
                for (slot, deco) in batch.iter_mut().zip(&decorated) {
                    *slot = deco.1;
                }
            }
            // Split the borrow: the `&mut` permutation alongside the shared
            // key columns, comparing through the same SortColumns order that
            // built it. `batch` is already in that order from the decorate-sort.
            let Self { perm_name, .. } = self;
            let cols = SortColumns::new(
                &self.dict_pool,
                &self.dict_off,
                &self.name_id,
                &self.size_lo,
                &self.size_ovf,
                &self.mtime,
            );
            merge_sorted_tail(perm_name, &batch, |a, b| cols.cmp_by(SortKey::Name, a, b));
        }
        self.content_generation += 1;
        verdict
    }

    /// Store the original spelling only when it differs from the folded
    /// bytes just appended at `lower_off` — the fold-identical majority
    /// costs nothing beyond the sentinel (ADR-0004).
    fn push_orig_if_differs(&mut self, folded_off: usize, orig: &[u8]) -> u32 {
        if orig == &self.dict_pool[folded_off..] {
            u32::MAX
        } else {
            // < MAX, not <=: u32::MAX is the fold-identical sentinel.
            assert!(
                self.orig_pool.len() + orig.len() < u32::MAX as usize,
                "orig pool overflow"
            );
            let off = self.orig_pool.len() as u32;
            self.orig_pool.extend_from_slice(orig);
            off
        }
    }

    /// Append a fresh dictionary entry for the folded bytes just written at
    /// `folded_off..` and return its `name_id`. Un-deduped on the hot path —
    /// `dedup_dict` collapses duplicates at `finish`/compaction (ADR-0032).
    fn push_dict_entry(&mut self, folded_off: usize) -> u32 {
        let name_id = self.dict_off.len() as u32;
        self.dict_off.push(folded_off as u32);
        self.dict_appends_since_dedup += 1;
        name_id
    }

    /// Append with a pre-resolved parent: the USN path resolves against the
    /// live index (see [`Self::upsert_synthetic`]); the initial-scan builder passes a
    /// provisional ROOT because `finish()` re-resolves every parent anyway —
    /// a per-push lookup against the unmerged FRN tail would be O(n²) there.
    pub(super) fn push_raw(&mut self, e: &RawEntry, parent: EntryId) -> EntryId {
        assert!(
            self.dict_pool.len() + e.name_utf16.len() * 4 < u32::MAX as usize,
            "name dictionary overflow"
        );
        let off = self.dict_pool.len();
        let mut orig = Vec::with_capacity(e.name_utf16.len() * 3);
        wtf8::push_wtf8_pair(e.name_utf16, &mut orig, &mut self.dict_pool);
        let name_id = self.push_dict_entry(off);
        let orig_off = self.push_orig_if_differs(off, &orig);
        self.push_columns(
            name_id,
            orig_off,
            parent,
            e.frn,
            e.size,
            e.mtime,
            e.is_dir,
            e.is_reparse,
            e.is_hidden,
            e.is_system,
        )
    }

    pub(super) fn push_encoded(&mut self, e: &EncodedEntry, parent: EntryId) -> EntryId {
        debug_assert_eq!(e.name_wtf8.len(), e.lower_wtf8.len());
        assert!(
            self.dict_pool.len() + e.lower_wtf8.len() < u32::MAX as usize,
            "name dictionary overflow"
        );
        let off = self.dict_pool.len();
        self.dict_pool.extend_from_slice(e.lower_wtf8);
        let name_id = self.push_dict_entry(off);
        let orig_off = self.push_orig_if_differs(off, e.name_wtf8);
        self.push_columns(
            name_id,
            orig_off,
            parent,
            e.frn,
            e.size,
            e.mtime,
            e.is_dir,
            e.is_reparse,
            e.is_hidden,
            e.is_system,
        )
    }

    /// Shared column append after the name bytes already landed in the
    /// dictionary as `name_id` and the original (if any) at `orig_off`. The
    /// flag/parent logic must stay identical between the utf16 (`push_raw`)
    /// and pre-encoded (`push_encoded`) entry points.
    #[allow(clippy::too_many_arguments)]
    fn push_columns(
        &mut self,
        name_id: u32,
        orig_off: u32,
        parent: EntryId,
        frn: Frn,
        size: u64,
        mtime: i64,
        is_dir: bool,
        is_reparse: bool,
        is_hidden: bool,
        is_system: bool,
    ) -> EntryId {
        assert!(
            self.len() < u32::MAX as usize - 1,
            "volume entry count overflow"
        );
        let id = self.len() as EntryId;
        self.name_id.push(name_id);
        self.orig_off.push(orig_off);
        self.parent.push(parent);
        self.push_size(size);
        self.mtime
            .push(crate::query::dates::mtime_ticks_to_secs(mtime));
        self.frn.push(frn.0);
        let mut f = 0u8;
        if is_dir {
            f |= flags::IS_DIR;
        }
        if is_reparse {
            f |= flags::REPARSE;
        }
        if is_hidden {
            f |= flags::HIDDEN;
        }
        if is_system {
            f |= flags::SYSTEM;
        }
        // Provisional during the initial scan (parents may resolve later —
        // the builder recomputes in finish()); exact on the USN path where
        // parents are already live.
        let parent_excluded = self
            .flag
            .get(parent as usize)
            .is_some_and(|pf| pf & flags::EXCLUDED != 0);
        if is_hidden || is_system || parent_excluded {
            f |= flags::EXCLUDED;
        }
        self.flag.push(f);
        id
    }

    /// Re-derive EXCLUDED for `id` from its own H/S bits and current parent.
    pub(super) fn recompute_excluded(&mut self, id: EntryId) {
        let p = self.parent[id as usize];
        let inherited = p != NO_PARENT && p != id && self.flag[p as usize] & flags::EXCLUDED != 0;
        let own = self.flag[id as usize] & (flags::HIDDEN | flags::SYSTEM) != 0;
        if own || inherited {
            self.flag[id as usize] |= flags::EXCLUDED;
        } else {
            self.flag[id as usize] &= !flags::EXCLUDED;
        }
    }

    /// Recompute inherited HIDDEN/SYSTEM exclusion for the complete forest.
    ///
    /// USN applies call this at most once per batch after any directory
    /// topology/attribute change. Each row enters and leaves the walk once, so
    /// moving a large subtree is O(index entries), never O(records × entries).
    /// Corrupt parent cycles are handled deterministically: the cycle is
    /// excluded iff any member carries its own HIDDEN/SYSTEM bit, and that
    /// state then propagates to descendants.
    pub(crate) fn recompute_all_excluded(&mut self) {
        const VISITING: u8 = 1;
        const DONE: u8 = 2;

        let n = self.len();
        let mut state = vec![0u8; n];
        let mut stack: Vec<EntryId> = Vec::new();

        for start in 0..n as EntryId {
            if state[start as usize] == DONE {
                continue;
            }
            stack.clear();
            let mut cur = start;
            let mut inherited = false;

            // A parent walk can visit at most every row before it must hit a
            // root, a completed row, or a cycle; one extra iteration observes
            // the terminal sentinel. Keep that input-derived bound independent
            // of the state-machine arms so a broken cycle branch cannot spin.
            for _ in 0..=n {
                if cur == NO_PARENT || cur as usize >= n {
                    break;
                }
                match state[cur as usize] {
                    DONE => {
                        inherited = self.is_excluded(cur);
                        break;
                    }
                    VISITING => {
                        let cycle_start = stack
                            .iter()
                            .position(|&id| id == cur)
                            .expect("VISITING entries belong to the current parent walk");
                        let cycle_excluded = stack[cycle_start..].iter().any(|&id| {
                            self.flag[id as usize] & (flags::HIDDEN | flags::SYSTEM) != 0
                        });
                        for &id in &stack[cycle_start..] {
                            if cycle_excluded {
                                self.flag[id as usize] |= flags::EXCLUDED;
                            } else {
                                self.flag[id as usize] &= !flags::EXCLUDED;
                            }
                            state[id as usize] = DONE;
                        }
                        stack.truncate(cycle_start);
                        inherited = cycle_excluded;
                        break;
                    }
                    _ => {
                        state[cur as usize] = VISITING;
                        stack.push(cur);
                        cur = self.parent[cur as usize];
                    }
                }
            }

            while let Some(id) = stack.pop() {
                let own = self.flag[id as usize] & (flags::HIDDEN | flags::SYSTEM) != 0;
                let excluded = own || inherited;
                if excluded {
                    self.flag[id as usize] |= flags::EXCLUDED;
                } else {
                    self.flag[id as usize] &= !flags::EXCLUDED;
                }
                state[id as usize] = DONE;
                inherited = excluded;
            }
        }
    }

    /// Update raw attribute bits (USN `BASIC_INFO_CHANGE`) and the derived
    /// EXCLUDED bit on every link of the object.
    pub fn update_attrs(
        &mut self,
        record: impl Into<RecordNo>,
        is_hidden: bool,
        is_system: bool,
    ) -> Option<EntryId> {
        let ids: Vec<_> = self.entries_by_record(record).collect();
        let first = ids.first().copied();
        for id in ids {
            self.update_attrs_id(id, is_hidden, is_system);
        }
        first
    }

    /// Refresh every object-owned attribute represented in the index.
    ///
    /// Returns whether at least one live link row changed, or `None` when the
    /// exact object generation is absent. The raw bits are shared by every
    /// hard link; the derived `EXCLUDED` bit is recomputed per path because
    /// each link can inherit from a different parent.
    pub(crate) fn update_object_attrs_frn(
        &mut self,
        frn: Frn,
        is_reparse: bool,
        is_hidden: bool,
        is_system: bool,
    ) -> Option<bool> {
        let ids: Vec<_> = self.entries_by_frn(frn).collect();
        if ids.is_empty() {
            return None;
        }
        let expected = if is_reparse { flags::REPARSE } else { 0 }
            | if is_hidden { flags::HIDDEN } else { 0 }
            | if is_system { flags::SYSTEM } else { 0 };
        let mask = flags::REPARSE | flags::HIDDEN | flags::SYSTEM;
        let changed = ids
            .iter()
            .any(|&id| self.flag[id as usize] & mask != expected);
        for id in ids {
            let flag = &mut self.flag[id as usize];
            *flag = (*flag & !flags::REPARSE) | if is_reparse { flags::REPARSE } else { 0 };
            self.update_attrs_id(id, is_hidden, is_system);
        }
        Some(changed)
    }

    fn update_attrs_id(&mut self, id: EntryId, is_hidden: bool, is_system: bool) {
        let old_raw = self.flag[id as usize] & (flags::HIDDEN | flags::SYSTEM);
        let f = &mut self.flag[id as usize];
        *f = (*f & !(flags::HIDDEN | flags::SYSTEM))
            | if is_hidden { flags::HIDDEN } else { 0 }
            | if is_system { flags::SYSTEM } else { 0 };
        self.recompute_excluded(id);
        let new_raw = self.flag[id as usize] & (flags::HIDDEN | flags::SYSTEM);
        if self.is_dir(id) && old_raw != new_raw {
            self.exclusion_tree_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::VolumeIndexBuilder;
    use crate::index::testutil::{build_hardlink_sample, build_sample, raw, raw_attr, u16s};

    const fn full(record: u64) -> Frn {
        Frn((1u64 << 48) | record)
    }

    /// The same MFT record as [`full`] under a different NTFS sequence number:
    /// a reference to a since-recycled generation of that record.
    const fn stale_generation(record: u64) -> Frn {
        Frn((2u64 << 48) | record)
    }

    fn strict_index() -> VolumeIndex {
        let mut builder = VolumeIndexBuilder::new_strict("C:", full(5)).expect("exact NTFS root");
        let a = u16s("a");
        let b = u16s("b");
        let file = u16s("file.txt");
        builder.push(RawEntry {
            parent_frn: full(5),
            frn: full(10),
            name_utf16: &a,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        builder.push(RawEntry {
            parent_frn: full(10),
            frn: full(20),
            name_utf16: &b,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        builder.push(RawEntry {
            parent_frn: full(10),
            frn: full(100),
            name_utf16: &file,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: 1,
        });
        builder.finish_strict().expect("strict fixture is rooted")
    }

    #[test]
    fn production_upsert_rejects_unknown_or_stale_parent_without_mutating() {
        let mut index = strict_index();
        let before_len = index.len();
        let before_live = index.live_len();
        let name = u16s("new.txt");
        let unknown = RawEntry {
            parent_frn: full(999),
            frn: full(200),
            name_utf16: &name,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: 1,
        };
        assert!(matches!(
            index.upsert_link_usn(&unknown),
            Err(IndexMutationError::UnresolvedParent { .. })
        ));

        let stale = RawEntry {
            parent_frn: stale_generation(10),
            ..unknown
        };
        assert!(matches!(
            index.upsert_link_usn(&stale),
            Err(IndexMutationError::UnresolvedParent { .. })
        ));
        assert_eq!(index.len(), before_len);
        assert_eq!(index.live_len(), before_live);
        assert!(index.entry_by_frn(full(200)).is_none());
    }

    #[test]
    fn production_directory_move_rejects_cycles_before_changing_the_row() {
        let mut index = strict_index();
        let directory = index.entry_by_frn(full(10)).expect("directory a");
        let old_parent = index.parent(directory);
        let old_name = index.name(directory).to_vec();

        assert!(matches!(
            index.rename_dir_frn_in_place(full(10), &u16s("moved"), full(20)),
            Err(IndexMutationError::ParentCycle { .. })
        ));
        assert_eq!(index.parent(directory), old_parent);
        assert_eq!(index.name(directory), old_name);
    }

    #[test]
    fn production_link_snapshot_rejects_duplicates_atomically() {
        let mut index = strict_index();
        let before_len = index.len();
        let before_live = index.live_len();
        let name = u16s("duplicate.txt");
        let duplicate = || RawEntry {
            parent_frn: full(10),
            frn: full(200),
            name_utf16: &name,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 7,
            mtime: 9,
        };
        assert!(matches!(
            index.reconcile_file_links_usn(full(200), &[duplicate(), duplicate()]),
            Err(IndexMutationError::DuplicateLink { .. })
        ));
        assert_eq!(index.len(), before_len);
        assert_eq!(index.live_len(), before_live);
    }

    #[test]
    fn production_batch_boundary_rejects_a_live_child_of_a_dead_parent() {
        let mut index = strict_index();
        let parent = index.entry_by_frn(full(10)).expect("directory a");
        index.tombstone_id(parent);
        assert!(matches!(
            index.merge_new_into_permutations(index.len() as EntryId),
            Err(IndexMutationError::InvalidTopology { .. })
        ));
    }

    /// Rejecting a batch is a message to the caller, not a licence to leave
    /// the index half-merged. A short `perm_name` makes the name order omit
    /// live rows, and an unbumped generation makes every derived cache keep
    /// answering for an index that no longer exists — both of them for the
    /// tens of seconds a rescan takes. `snapshot.rs` refuses to *load* an
    /// incomplete permutation; the live index must never hold one either.
    #[test]
    fn a_rejected_batch_boundary_still_leaves_one_complete_permutation() {
        let mut index = strict_index();
        let generation = index.content_generation();
        let first_new = index.len() as EntryId;

        let name = u16s("appended.txt");
        index
            .upsert_link_usn(&RawEntry {
                parent_frn: full(20),
                frn: full(200),
                name_utf16: &name,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: 4,
                mtime: 5,
            })
            .expect("the fixture parent is exact");
        // Now break the topology the boundary validates: `b` keeps a live
        // child while it is itself dead.
        let directory = index.entry_by_frn(full(20)).expect("directory b");
        index.tombstone_id(directory);

        assert!(matches!(
            index.merge_new_into_permutations(first_new),
            Err(IndexMutationError::InvalidTopology { .. })
        ));

        let permutation = index.name_permutation();
        assert_eq!(
            permutation.len(),
            index.len(),
            "every appended row reached the name permutation"
        );
        let mut seen: Vec<EntryId> = permutation.to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..index.len() as EntryId).collect::<Vec<_>>());
        for pair in permutation.windows(2) {
            assert!(index.cmp_by(SortKey::Name, pair[0], pair[1]).is_lt());
        }
        assert_eq!(
            index.content_generation(),
            generation + 1,
            "derived caches must be told the rows changed"
        );
    }

    #[test]
    fn link_mutations_preserve_siblings_and_object_updates_reach_all_links() {
        let mut idx = build_hardlink_sample();
        let object = Frn((1u64 << 48) | 0x64);
        let parent_a = Frn((1u64 << 48) | 0x0A);
        let parent_b = Frn((1u64 << 48) | 0x14);
        let updated_mtime =
            crate::query::dates::FILETIME_UNIX_EPOCH + 77 * crate::query::dates::TICKS_PER_SECOND;

        let first_new = idx.len() as u32;
        let refreshed_name = u16s("shared.txt");
        let refreshed = RawEntry {
            parent_frn: parent_a,
            frn: object,
            name_utf16: &refreshed_name,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 99,
            mtime: 9,
        };
        idx.upsert_link_usn(&refreshed)
            .expect("fixture parent is exact");
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(idx.entries_by_frn(object).count(), 2);
        assert!(
            idx.entry_by_link(object, parent_b, &u16s("alias.txt"))
                .is_some(),
            "refreshing one link must retain its sibling"
        );

        idx.update_stat_frn(object, 1234, updated_mtime).unwrap();
        idx.update_object_attrs_frn(object, false, true, false)
            .unwrap();
        for id in idx.entries_by_frn(object) {
            assert_eq!(idx.size(id), 1234);
            assert_eq!(idx.mtime(id), updated_mtime);
            assert!(idx.is_excluded(id));
        }

        assert!(
            idx.delete_link_frn(object, parent_a, &refreshed_name)
                .is_some()
        );
        let remaining: Vec<_> = idx.entries_by_frn(object).collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(idx.name(remaining[0]), b"alias.txt");

        idx.delete_frn(object).unwrap();
        assert_eq!(idx.entries_by_frn(object).count(), 0);
    }

    #[test]
    fn record_reuse_retires_every_link_of_the_old_sequence() {
        let mut idx = build_hardlink_sample();
        let old = Frn((1u64 << 48) | 0x64);
        assert_eq!(idx.entries_by_frn(old).count(), 2);

        let name = u16s("reused.txt");
        let new = RawEntry {
            parent_frn: Frn((1u64 << 48) | 0x0A),
            frn: Frn((2u64 << 48) | 0x64),
            name_utf16: &name,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: 1,
        };
        idx.upsert_link_usn(&new).expect("fixture parent is exact");

        assert_eq!(idx.entries_by_frn(old).count(), 0);
        assert_eq!(idx.entries_by_frn(new.frn).count(), 1);
    }

    #[test]
    fn complete_link_snapshot_is_reconciled_by_exact_parent_and_name() {
        let mut idx = build_hardlink_sample();
        let object = Frn((1u64 << 48) | 0x64);
        let parent_a = Frn((1u64 << 48) | 0x0A);
        let parent_b = Frn((1u64 << 48) | 0x14);
        let shared = u16s("shared.txt");
        let third = u16s("third.txt");
        let retained_before = idx
            .entry_by_link(object, parent_a, &shared)
            .expect("existing link");
        let metadata_time =
            crate::query::dates::FILETIME_UNIX_EPOCH + 9 * crate::query::dates::TICKS_PER_SECOND;
        let desired = [
            RawEntry {
                parent_frn: parent_a,
                frn: object,
                name_utf16: &shared,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: 500,
                mtime: metadata_time,
            },
            RawEntry {
                parent_frn: parent_b,
                frn: object,
                name_utf16: &third,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: 500,
                mtime: metadata_time,
            },
        ];

        let first_new = idx.len() as u32;
        let changed = idx
            .reconcile_file_links_usn(object, &desired)
            .expect("fixture snapshot is complete");
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(
            changed,
            LinkReconcileStats {
                added: 1,
                removed: 1,
                retained: 1,
                metadata_changed: true,
            }
        );
        assert_eq!(
            idx.entry_by_link(object, parent_a, &shared),
            Some(retained_before),
            "an unchanged link keeps its EntryId"
        );
        assert!(
            idx.entry_by_link(object, parent_b, &u16s("alias.txt"))
                .is_none()
        );
        assert!(
            idx.entry_by_link(object, parent_b, &third).is_some(),
            "the authoritative new link is appended"
        );
        for id in idx.entries_by_frn(object) {
            assert_eq!((idx.size(id), idx.mtime(id)), (500, metadata_time));
        }
    }

    #[test]
    fn rename_is_tombstone_plus_new_entry() {
        let mut idx = build_sample();
        let old = idx.entry_by_record(100).unwrap();
        let first_new = idx.len() as u32;
        let renamed = u16s("renamed.txt");
        let mut e = raw(100, 50, &renamed, false, 10, 300);
        e.frn = idx.frn(old); // same FRN, new name
        let new_id = idx.upsert_synthetic(&e);
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert!(!idx.is_live(old));
        assert!(idx.is_live(new_id));
        assert_eq!(idx.entry_by_record(100), Some(new_id));
        assert_eq!(idx.name(new_id), b"renamed.txt");
        // The name permutation contains the new id in sorted position.
        let pos = idx
            .name_permutation()
            .iter()
            .position(|&i| i == new_id)
            .unwrap();
        let perm = idx.name_permutation();
        if pos > 0 {
            assert!(idx.lower_name(perm[pos - 1]) <= idx.lower_name(new_id));
        }
        if pos + 1 < perm.len() {
            assert!(idx.lower_name(new_id) <= idx.lower_name(perm[pos + 1]));
        }
    }

    #[test]
    fn delete_and_reparent() {
        let mut idx = build_sample();
        let big = idx.entry_by_record(60).unwrap();
        idx.reparent_synthetic(60, 50);
        let docs = idx.entry_by_record(50).unwrap();
        assert_eq!(idx.parent(big), docs);

        idx.delete(60);
        assert!(!idx.is_live(big));
        assert_eq!(idx.entry_by_record(60), None);
        assert!(idx.tombstone_ratio() > 0.0);
    }

    #[test]
    fn usn_insert_and_moves_track_exclusion() {
        let sysdir = u16s("sysdir");
        let normal = u16s("docs");
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        b.push(raw_attr(10, 5, &sysdir, true, false, true));
        b.push(raw_attr(20, 5, &normal, true, false, false));
        let mut idx = b.finish();

        // New plain file created under the system dir → inherits.
        let name = u16s("payload.tmp");
        let first_new = idx.len() as u32;
        let id = idx.upsert_synthetic(&raw_attr(30, 10, &name, false, false, false));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert!(idx.is_excluded(id));

        // Moved out into a normal dir → bit clears.
        idx.reparent_synthetic(30, 20);
        assert!(!idx.is_excluded(id));

        // Attribute change marks it hidden → re-excluded.
        idx.update_attrs(30, true, false);
        assert!(idx.is_excluded(id));
    }

    /// `perm_name` must stay a sorted permutation of every entry id.
    fn assert_perm_name_sorted(idx: &VolumeIndex) {
        let perm = idx.name_permutation();
        assert_eq!(perm.len(), idx.len(), "perm_name must cover every entry");
        let mut seen: Vec<EntryId> = perm.to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..idx.len() as u32).collect::<Vec<_>>());
        for w in perm.windows(2) {
            assert!(
                idx.cmp_by(SortKey::Name, w[0], w[1]).is_lt(),
                "perm_name out of order at {w:?}"
            );
        }
    }

    #[test]
    fn rename_dir_in_place_keeps_permutation_sorted() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let (alpha, mike, zulu, child) = (u16s("alpha"), u16s("mike"), u16s("zulu"), u16s("a.txt"));
        b.push(raw(10, 5, &alpha, true, 0, 1));
        b.push(raw(20, 5, &mike, true, 0, 2));
        b.push(raw(30, 5, &zulu, true, 0, 3));
        b.push(raw(11, 10, &child, false, 1, 4));
        let mut idx = b.finish();
        let dir = idx.entry_by_record(10).unwrap();

        // Move toward the end of the name order, then to the front.
        let zz = u16s("zz_renamed");
        assert_eq!(idx.rename_dir_synthetic_in_place(10, &zz, 5), Some(dir));
        assert_eq!(idx.name(dir), b"zz_renamed");
        assert_perm_name_sorted(&idx);
        let first = u16s("0_first");
        assert_eq!(idx.rename_dir_synthetic_in_place(10, &first, 5), Some(dir));
        assert_perm_name_sorted(&idx);

        // In place: same EntryId, no tombstone, children follow lazily.
        assert_eq!(idx.entry_by_record(10), Some(dir));
        assert_eq!(idx.len(), 5);
        assert_eq!(idx.live_len(), 5);
        let c = idx.entry_by_record(11).unwrap();
        let mut p = Vec::new();
        idx.append_path(c, &mut p).unwrap();
        assert_eq!(p, b"C:\\0_first\\a.txt");
        // Name renames never touch sizes/mtimes, so the lazy size/mtime
        // orders (query::memo) stay valid without any signal from here.
    }

    #[test]
    fn mutations_on_unknown_records_are_safe_noops() {
        let mut idx = build_sample();
        let generation = idx.content_generation();
        let perm_before = idx.name_permutation().to_vec();
        let ghost = u16s("ghost");
        assert_eq!(idx.rename_dir_synthetic_in_place(9999, &ghost, 5), None);
        assert_eq!(idx.delete(9999), None);
        assert_eq!(idx.update_stat(9999, 1, 1), None);
        assert_eq!(idx.update_attrs(9999, true, true), None);
        assert_eq!(idx.reparent_synthetic(9999, 5), None);
        assert_eq!(idx.len(), 4);
        assert_eq!(idx.live_len(), 4);
        assert_eq!(idx.name_permutation(), perm_before.as_slice());
        assert_eq!(idx.content_generation(), generation);
    }

    #[test]
    fn rename_dir_with_itself_as_parent_keeps_current_parent() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let (top, sub) = (u16s("top"), u16s("sub"));
        b.push(raw(10, 5, &top, true, 0, 1));
        b.push(raw(20, 10, &sub, true, 0, 2));
        let mut idx = b.finish();
        let top_id = idx.entry_by_record(10).unwrap();
        let sub_id = idx.entry_by_record(20).unwrap();

        // new_parent_record == own record: the parent write is guarded, no
        // self-cycle is created and the path chain still terminates.
        let renamed = u16s("renamed");
        assert_eq!(
            idx.rename_dir_synthetic_in_place(20, &renamed, 20),
            Some(sub_id)
        );
        assert_eq!(idx.parent(sub_id), top_id);
        let mut p = Vec::new();
        idx.append_path(sub_id, &mut p).unwrap();
        assert_eq!(p, b"C:\\top\\renamed");
        assert_perm_name_sorted(&idx);

        // Unknown new parent attaches to the root (current pinned behavior,
        // same as push_raw's orphan handling).
        let renamed2 = u16s("renamed2");
        assert_eq!(
            idx.rename_dir_synthetic_in_place(20, &renamed2, 424_242),
            Some(sub_id)
        );
        assert_eq!(idx.parent(sub_id), VolumeIndex::ROOT);
    }

    #[test]
    fn reparent_to_self_keeps_current_parent() {
        // A corrupt USN record whose parent FRN equals its own FRN must not
        // create a self-cycle (same guard as rename_dir_in_place).
        let mut idx = build_sample();
        let docs = idx.entry_by_record(50).unwrap();
        let before = idx.parent(docs);
        assert_eq!(idx.reparent_synthetic(50, 50), Some(docs));
        assert_eq!(idx.parent(docs), before);
    }

    #[test]
    fn update_attrs_recomputes_excluded_from_own_and_inherited_bits() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let (sysdir, plain, f, g) = (u16s("sysdir"), u16s("plain"), u16s("f.txt"), u16s("g.txt"));
        b.push(raw_attr(10, 5, &sysdir, true, false, true));
        b.push(raw_attr(20, 5, &plain, true, false, false));
        b.push(raw_attr(30, 20, &f, false, false, false));
        b.push(raw_attr(40, 20, &g, false, false, false));
        let mut idx = b.finish();
        let f_id = idx.entry_by_record(30).unwrap();
        let g_id = idx.entry_by_record(40).unwrap();
        assert!(!idx.is_excluded(f_id));

        // Own hidden bit set → excluded; cleared again → plain.
        idx.update_attrs(30, true, false).unwrap();
        assert!(idx.is_excluded(f_id));
        idx.update_attrs(30, false, false).unwrap();
        assert!(!idx.is_excluded(f_id));

        // Under an excluded parent, clearing own bits keeps the inherited bit.
        idx.reparent_synthetic(30, 10).unwrap();
        assert!(idx.is_excluded(f_id));
        idx.update_attrs(30, false, false).unwrap();
        assert!(idx.is_excluded(f_id));

        // Marking a dir hidden updates it immediately and propagates through
        // every existing descendant exactly once at the batch boundary.
        let plain_id = idx.entry_by_record(20).unwrap();
        idx.update_attrs(20, true, false).unwrap();
        assert!(idx.is_excluded(plain_id));
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert!(idx.is_excluded(g_id));

        // New entries created under it inherit immediately.
        let h = u16s("h.txt");
        let first_new = idx.len() as u32;
        let h_id = idx.upsert_synthetic(&raw_attr(50, 20, &h, false, false, false));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert!(idx.is_excluded(h_id));

        // Clearing the directory attribute clears inherited exclusion from
        // both old and newly-created descendants at the next boundary.
        idx.update_attrs(20, false, false).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert!(!idx.is_excluded(g_id));
        assert!(!idx.is_excluded(h_id));
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    /// Random create/rename/delete/stat batches through the in-place merge:
    /// every permutation stays a complete permutation, `perm_name` stays
    /// strictly sorted (names are never mutated in place), and every record
    /// resolves per a side model — both mid-batch (tail scan) and after the
    /// merge (sorted lookup).
    #[test]
    fn random_batches_keep_permutations_canonical_and_lookups_model_true() {
        use std::collections::HashMap;
        let mut rng = Rng(0x5EED_CAFE_D00D);
        let mut idx = build_sample();
        // record → the live entry's expected name (None = deleted).
        let mut model: HashMap<u64, Option<Vec<u8>>> = HashMap::new();
        for record in [50u64, 60, 100] {
            let id = idx.entry_by_record(record).unwrap();
            model.insert(record, Some(idx.name(id).to_vec()));
        }
        let check = |idx: &VolumeIndex, record: u64, expect: &Option<Vec<u8>>| match (
            idx.entry_by_record(record),
            expect,
        ) {
            (Some(id), Some(name)) => assert_eq!(idx.name(id), &name[..]),
            (None, None) => {}
            (got, want) => panic!("record {record}: got {got:?}, want live={}", want.is_some()),
        };

        for _ in 0..100 {
            let first_new = idx.len() as u32;
            for _ in 0..=(rng.next() % 8) {
                let record = 100 + rng.next() % 30;
                match rng.next() % 4 {
                    0 | 1 => {
                        let name = format!("n{}_{}.txt", record, rng.next() % 100);
                        let units = u16s(&name);
                        idx.upsert_synthetic(&raw(
                            record,
                            50,
                            &units,
                            false,
                            rng.next() % 1000,
                            (rng.next() % 1000) as i64,
                        ));
                        model.insert(record, Some(name.into_bytes()));
                    }
                    2 => {
                        idx.delete(record);
                        model.insert(record, None);
                    }
                    _ => {
                        // In-place stat update: never repositions an entry
                        // (pinned behavior); names unaffected. Mix sizes on
                        // both sides of the u32 overflow sentinel.
                        let size = if rng.next().is_multiple_of(8) {
                            (4u64 << 30) + rng.next() % 1000
                        } else {
                            rng.next() % 5000
                        };
                        idx.update_stat(record, size, (rng.next() % 5000) as i64);
                    }
                }
                if let Some(expect) = model.get(&record) {
                    check(&idx, record, expect); // unmerged-tail resolution
                }
            }
            idx.merge_new_into_permutations(first_new)
                .expect("fixture topology remains valid");
            // Permutation property: every id exactly once, strictly sorted
            // (names are never mutated in place). The lazy size/mtime
            // orders are covered by query::memo's SortPerm oracle.
            let mut seen: Vec<EntryId> = idx.name_permutation().to_vec();
            seen.sort_unstable();
            assert_eq!(seen, (0..idx.len() as u32).collect::<Vec<_>>());
            assert_perm_name_sorted(&idx);
            for (record, expect) in &model {
                check(&idx, *record, expect);
            }
        }
    }

    /// `name()` must return the exact WTF-8 input bytes through every write
    /// path and a snapshot roundtrip — the fold-overflow layout (originals
    /// stored only where they differ) must be invisible to readers.
    #[test]
    fn names_roundtrip_byte_exact_through_fold_overflow_layout() {
        let cases: &[&str] = &[
            "lowercase.txt",
            "File.TXT",
            "ALLCAPS",
            "日本語ファイル.txt",
            "ΣΟΦΟΣ.doc",
            "İstanbul.log",
            "Mixed日本語Name.TXT",
            "𠮷野家🦀.txt",
        ];
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        for (i, name) in cases.iter().enumerate() {
            let units = u16s(name);
            b.push(raw(100 + i as u64, 5, &units, false, 1, 1));
        }
        let mut idx = b.finish();
        // Lone surrogate through the USN write path.
        let first_new = idx.len() as u32;
        idx.upsert_synthetic(&raw(900, 5, &[0x0041, 0xD800, 0x0042], false, 1, 1));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        let check = |idx: &VolumeIndex| {
            for (i, name) in cases.iter().enumerate() {
                let id = idx.entry_by_record(100 + i as u64).unwrap();
                assert_eq!(idx.name(id), name.as_bytes(), "{name}");
                assert_eq!(idx.name(id).len(), idx.lower_name(id).len(), "{name}");
            }
            let id = idx.entry_by_record(900).unwrap();
            let mut units = Vec::new();
            crate::wtf8::wtf8_to_utf16(idx.name(id), &mut units);
            assert_eq!(units, vec![0x0041, 0xD800, 0x0042]);
        };
        check(&idx);

        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 1).unwrap();
        let (loaded, _, _) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
        check(&loaded);
    }

    /// `dedup_orig` (ADR-0033 Lever 1) stores each distinct original once: a
    /// volume full of repeated capitalized names keeps a single `orig_pool`
    /// copy of each, while every entry's `name()` still reads back its exact
    /// original bytes and fold-identical names own no copy at all.
    #[test]
    fn dedup_orig_stores_each_original_once() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        // 3×README + 2×Makefile differ from their fold; the two lowercase
        // names are fold-identical and own no original copy.
        let names = [
            "README",
            "README",
            "README",
            "Makefile",
            "Makefile",
            "notes.txt",
            "data.bin",
        ];
        for (i, name) in names.iter().enumerate() {
            let units = u16s(name);
            b.push(raw(100 + i as u64, 5, &units, false, 1, 1));
        }
        let idx = b.finish();

        // Every entry reads back its exact original through the shared copy.
        for (i, name) in names.iter().enumerate() {
            let id = idx.entry_by_record(100 + i as u64).unwrap();
            assert_eq!(idx.name(id), name.as_bytes(), "{name}");
        }
        // The differing originals are deduped to one copy each: root "C:"
        // (folds to "c:") + "README" + "Makefile" = 2 + 6 + 8 = 16 bytes, not
        // the un-deduped 2 + 6*3 + 8*2 = 36. The fold-identical names add none.
        assert_eq!(idx.orig_pool.len(), 16);
    }

    /// In-place dir renames cross the fold-identity boundary in both
    /// directions: gaining an original copy and dropping back to the
    /// shared folded bytes.
    #[test]
    fn dir_rename_crosses_fold_identity_both_ways() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let plain = u16s("plain");
        b.push(raw(10, 5, &plain, true, 0, 1));
        let mut idx = b.finish();
        let id = idx.entry_by_record(10).unwrap();
        assert_eq!(idx.name(id), b"plain");
        assert_eq!(idx.lower_name(id), b"plain");

        let upper = u16s("Upper");
        idx.rename_dir_synthetic_in_place(10, &upper, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert_eq!(idx.name(id), b"Upper");
        assert_eq!(idx.lower_name(id), b"upper");

        let back = u16s("back_to_lower");
        idx.rename_dir_synthetic_in_place(10, &back, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert_eq!(idx.name(id), b"back_to_lower");
        assert_eq!(idx.lower_name(id), b"back_to_lower");
        assert_perm_name_sorted(&idx);
    }

    /// Sizes round-trip across the u32 column + overflow map in both
    /// directions (grow past the sentinel, shrink back under it).
    #[test]
    fn size_overflow_roundtrips_through_updates() {
        let mut idx = build_sample();
        let first_new = idx.len() as u32;
        let name = u16s("huge.vhdx");
        let id = idx.upsert_synthetic(&raw(900, 50, &name, false, (6u64 << 30) + 7, 1));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(idx.size(id), (6u64 << 30) + 7);

        // Shrink under the sentinel: the overflow slot must be reclaimed.
        idx.update_stat(900, 1234, 2).unwrap();
        assert_eq!(idx.size(id), 1234);

        // Grow back over it; exactly u32::MAX must overflow too (sentinel).
        idx.update_stat(900, u32::MAX as u64, 3).unwrap();
        assert_eq!(idx.size(id), u32::MAX as u64);
        idx.update_stat(900, u64::MAX, 4).unwrap();
        assert_eq!(idx.size(id), u64::MAX);
    }

    /// `dead_name_bytes` follows the reclaimable original-spelling copies a
    /// rebuild drops (ADR-0032): folded bytes are shared in the dictionary, so
    /// only a non-fold-identical entry ("Note.TXT") owns reclaimable bytes —
    /// the all-lowercase names own none. Snapshot restore recomputes the
    /// tombstone share (rename gaps are a lost lower bound).
    #[test]
    fn dead_name_bytes_tracks_pool_garbage() {
        let owned = |idx: &VolumeIndex, record: u64| {
            let id = idx.entry_by_record(record).unwrap();
            if idx.name(id) == idx.lower_name(id) {
                0 // fold-identical: nothing owned (folded bytes are shared)
            } else {
                idx.name(id).len() as u64 // only the original copy is owned
            }
        };
        let mut idx = build_sample();
        assert_eq!(idx.stats("C:").dead_name_bytes, 0);

        let note = owned(&idx, 100); // "Note.TXT": its original copy
        assert_eq!(note, 8);
        let first_new = idx.len() as u32;
        let renamed = u16s("renamed.txt");
        idx.upsert_synthetic(&raw(100, 50, &renamed, false, 1, 1));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(idx.stats("C:").dead_name_bytes, note);

        let big = owned(&idx, 60); // "big.bin": fold-identical, owns nothing
        assert_eq!(big, 0);
        idx.delete(60);
        assert_eq!(idx.stats("C:").dead_name_bytes, note + big);

        let docs = owned(&idx, 50);
        let dir2 = u16s("docs2");
        idx.rename_dir_synthetic_in_place(50, &dir2, 5);
        let s = idx.stats("C:");
        assert_eq!(s.dead_name_bytes, note + big + docs);
        assert!(s.pool_garbage_ratio > 0.0);

        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 1).unwrap();
        let (loaded, _, _) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
        assert_eq!(loaded.stats("C:").dead_name_bytes, note + big);
    }

    #[test]
    fn no_op_batches_keep_content_generation_monotonic() {
        let mut idx = build_sample();
        let g0 = idx.content_generation();
        let s0 = idx.structural_generation();
        let perm_before = idx.name_permutation().to_vec();

        // Empty batch (e.g. a dir-rename-only USN batch): generation still
        // moves so derived caches invalidate, permutations stay put.
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert_eq!(idx.content_generation(), g0 + 1);
        assert_eq!(idx.name_permutation(), perm_before.as_slice());

        // Tombstone-only batch: ids stay in the permutations (flag-only).
        idx.delete(60);
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert_eq!(idx.content_generation(), g0 + 2);
        assert_eq!(idx.name_permutation(), perm_before.as_slice());

        // Individual mutations between batches never bump on their own.
        idx.update_stat(100, 1, 1).unwrap();
        idx.update_attrs(100, true, false).unwrap();
        assert_eq!(idx.content_generation(), g0 + 2);
        // Content batches never touch the structural generation.
        assert_eq!(idx.structural_generation(), s0);
    }
}
