use crate::wtf8;

use super::{EntryId, NO_PARENT, RawEntry, SortKey, VolumeIndex, flags, masked};

impl VolumeIndex {
    // ── Incremental mutation (USN batches; see module docs) ──────────────

    /// Insert or replace an entry for `record`. Replacement (same record
    /// number) tombstones the old entry — this is how renames work.
    /// Returns the new id. Caller must finish the batch with
    /// [`Self::merge_new_into_permutations`].
    pub fn upsert(&mut self, e: &RawEntry) -> EntryId {
        if let Some(old) = self.frn_map.get(&masked(e.record)).copied() {
            self.flag[old as usize] |= flags::TOMBSTONE;
            self.tombstones += 1;
        }
        let id = self.push_raw(e);
        self.frn_map.insert(masked(e.record), id);
        id
    }

    pub fn delete(&mut self, record: u64) -> Option<EntryId> {
        let id = self.frn_map.remove(&masked(record))?;
        self.flag[id as usize] |= flags::TOMBSTONE;
        self.tombstones += 1;
        Some(id)
    }

    /// Move `record` under a new parent. Cheap: no permutation depends on
    /// the path, and child paths rebuild lazily. A corrupt record naming
    /// itself as parent keeps its current parent (no self-cycles).
    pub fn reparent(&mut self, record: u64, new_parent_record: u64) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let parent = self
            .entry_by_record(new_parent_record)
            .unwrap_or(Self::ROOT);
        if parent != id {
            self.parent[id as usize] = parent;
        }
        self.recompute_excluded(id);
        Some(id)
    }

    /// Rename/move a *directory* in place. Directories must keep their
    /// EntryId stable — children's `parent` fields point at it — so instead
    /// of tombstone+new (the file path), the name is swapped and the entry is
    /// repositioned inside `perm_name`. O(len) per rename; directory renames
    /// are rare enough that this beats invalidating every child.
    pub fn rename_dir_in_place(
        &mut self,
        record: u64,
        name_utf16: &[u16],
        new_parent_record: u64,
    ) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let pos = self.perm_name.iter().position(|&x| x == id)?;
        self.perm_name.remove(pos);

        let off = self.name_pool.len();
        wtf8::push_wtf8_pair(name_utf16, &mut self.name_pool, &mut self.lower_pool);
        self.name_off[id as usize] = off as u32;
        self.name_len[id as usize] = (self.name_pool.len() - off) as u16;
        let parent = self
            .entry_by_record(new_parent_record)
            .unwrap_or(Self::ROOT);
        if parent != id {
            self.parent[id as usize] = parent;
        }
        self.recompute_excluded(id);

        let ins = self
            .perm_name
            .binary_search_by(|&x| self.cmp_by(SortKey::Name, x, id))
            .unwrap_or_else(|e| e);
        self.perm_name.insert(ins, id);
        Some(id)
    }

    /// Update size/mtime in place (USN_REASON_DATA_* without a name change).
    pub fn update_stat(&mut self, record: u64, size: u64, mtime: i64) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        self.size[id as usize] = size;
        self.mtime[id as usize] = mtime;
        Some(id)
    }

    /// Merge entries `first_new..len` (already appended, unsorted) into all
    /// permutation arrays in one pass per key, then bump the content
    /// generation. Call once per USN batch.
    pub fn merge_new_into_permutations(&mut self, first_new: EntryId) {
        let new_ids: Vec<EntryId> = (first_new..self.len() as u32).collect();
        if !new_ids.is_empty() {
            for key in [SortKey::Name, SortKey::Size, SortKey::Mtime] {
                let mut batch = new_ids.clone();
                batch.sort_unstable_by(|&a, &b| self.cmp_by(key, a, b));
                let merged = {
                    let old = self.permutation(key);
                    let mut merged = Vec::with_capacity(old.len() + batch.len());
                    let (mut i, mut j) = (0, 0);
                    while i < old.len() && j < batch.len() {
                        if self.cmp_by(key, old[i], batch[j]).is_le() {
                            merged.push(old[i]);
                            i += 1;
                        } else {
                            merged.push(batch[j]);
                            j += 1;
                        }
                    }
                    merged.extend_from_slice(&old[i..]);
                    merged.extend_from_slice(&batch[j..]);
                    merged
                };
                match key {
                    SortKey::Name => self.perm_name = merged,
                    SortKey::Size => self.perm_size = merged,
                    SortKey::Mtime => self.perm_mtime = merged,
                }
            }
        }
        self.content_generation += 1;
    }

    pub(super) fn push_raw(&mut self, e: &RawEntry) -> EntryId {
        assert!(
            self.len() < u32::MAX as usize - 1,
            "volume entry count overflow"
        );
        let id = self.len() as EntryId;
        let off = self.name_pool.len();
        assert!(
            off + e.name_utf16.len() * 4 < u32::MAX as usize,
            "name pool overflow"
        );
        wtf8::push_wtf8_pair(e.name_utf16, &mut self.name_pool, &mut self.lower_pool);
        self.name_off.push(off as u32);
        self.name_len.push((self.name_pool.len() - off) as u16);
        // Parent is resolved against the map; unknown parents attach to root
        // (orphan records do occur in real MFTs).
        let parent = self
            .frn_map
            .get(&masked(e.parent_record))
            .copied()
            .unwrap_or(Self::ROOT);
        self.parent.push(parent);
        self.size.push(e.size);
        self.mtime.push(e.mtime);
        self.frn.push(e.frn);
        let mut f = 0u8;
        if e.is_dir {
            f |= flags::IS_DIR;
        }
        if e.is_reparse {
            f |= flags::REPARSE;
        }
        if e.is_hidden {
            f |= flags::HIDDEN;
        }
        if e.is_system {
            f |= flags::SYSTEM;
        }
        // Provisional during the initial scan (parents may resolve later —
        // the builder recomputes in finish()); exact on the USN path where
        // parents are already live.
        let parent_excluded = self
            .flag
            .get(parent as usize)
            .is_some_and(|pf| pf & flags::EXCLUDED != 0);
        if e.is_hidden || e.is_system || parent_excluded {
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

    /// Update raw attribute bits (USN BASIC_INFO_CHANGE) and the derived
    /// EXCLUDED bit.
    pub fn update_attrs(
        &mut self,
        record: u64,
        is_hidden: bool,
        is_system: bool,
    ) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let f = &mut self.flag[id as usize];
        *f = (*f & !(flags::HIDDEN | flags::SYSTEM))
            | if is_hidden { flags::HIDDEN } else { 0 }
            | if is_system { flags::SYSTEM } else { 0 };
        self.recompute_excluded(id);
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::VolumeIndexBuilder;
    use crate::index::testutil::{build_sample, raw, raw_attr, u16s};

    #[test]
    fn rename_is_tombstone_plus_new_entry() {
        let mut idx = build_sample();
        let old = idx.entry_by_record(100).unwrap();
        let first_new = idx.len() as u32;
        let renamed = u16s("renamed.txt");
        let mut e = raw(100, 50, &renamed, false, 10, 300);
        e.frn = idx.frn(old); // same FRN, new name
        let new_id = idx.upsert(&e);
        idx.merge_new_into_permutations(first_new);

        assert!(!idx.is_live(old));
        assert!(idx.is_live(new_id));
        assert_eq!(idx.entry_by_record(100), Some(new_id));
        assert_eq!(idx.name(new_id), b"renamed.txt");
        // Permutations contain the new id in sorted position.
        let pos = idx
            .permutation(SortKey::Name)
            .iter()
            .position(|&i| i == new_id)
            .unwrap();
        let perm = idx.permutation(SortKey::Name);
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
        idx.reparent(60, 50);
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
        let mut b = VolumeIndexBuilder::new("C:", 5);
        b.push(raw_attr(10, 5, &sysdir, true, false, true));
        b.push(raw_attr(20, 5, &normal, true, false, false));
        let mut idx = b.finish();

        // New plain file created under the system dir → inherits.
        let name = u16s("payload.tmp");
        let first_new = idx.len() as u32;
        let id = idx.upsert(&raw_attr(30, 10, &name, false, false, false));
        idx.merge_new_into_permutations(first_new);
        assert!(idx.is_excluded(id));

        // Moved out into a normal dir → bit clears.
        idx.reparent(30, 20);
        assert!(!idx.is_excluded(id));

        // Attribute change marks it hidden → re-excluded.
        idx.update_attrs(30, true, false);
        assert!(idx.is_excluded(id));
    }

    /// perm_name must stay a sorted permutation of every entry id.
    fn assert_perm_name_sorted(idx: &VolumeIndex) {
        let perm = idx.permutation(SortKey::Name);
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
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (alpha, mike, zulu, child) = (u16s("alpha"), u16s("mike"), u16s("zulu"), u16s("a.txt"));
        b.push(raw(10, 5, &alpha, true, 0, 1));
        b.push(raw(20, 5, &mike, true, 0, 2));
        b.push(raw(30, 5, &zulu, true, 0, 3));
        b.push(raw(11, 10, &child, false, 1, 4));
        let mut idx = b.finish();
        let dir = idx.entry_by_record(10).unwrap();
        let size_before = idx.permutation(SortKey::Size).to_vec();
        let mtime_before = idx.permutation(SortKey::Mtime).to_vec();

        // Move toward the end of the name order, then to the front.
        let zz = u16s("zz_renamed");
        assert_eq!(idx.rename_dir_in_place(10, &zz, 5), Some(dir));
        assert_eq!(idx.name(dir), b"zz_renamed");
        assert_perm_name_sorted(&idx);
        let first = u16s("0_first");
        assert_eq!(idx.rename_dir_in_place(10, &first, 5), Some(dir));
        assert_perm_name_sorted(&idx);

        // In place: same EntryId, no tombstone, children follow lazily.
        assert_eq!(idx.entry_by_record(10), Some(dir));
        assert_eq!(idx.len(), 5);
        assert_eq!(idx.live_len(), 5);
        let c = idx.entry_by_record(11).unwrap();
        let mut p = Vec::new();
        idx.append_path(c, &mut p);
        assert_eq!(p, b"C:\\0_first\\a.txt");
        // Name renames never touch the size/mtime orders.
        assert_eq!(idx.permutation(SortKey::Size), size_before.as_slice());
        assert_eq!(idx.permutation(SortKey::Mtime), mtime_before.as_slice());
    }

    #[test]
    fn mutations_on_unknown_records_are_safe_noops() {
        let mut idx = build_sample();
        let generation = idx.content_generation();
        let perm_before = idx.permutation(SortKey::Name).to_vec();
        let ghost = u16s("ghost");
        assert_eq!(idx.rename_dir_in_place(9999, &ghost, 5), None);
        assert_eq!(idx.delete(9999), None);
        assert_eq!(idx.update_stat(9999, 1, 1), None);
        assert_eq!(idx.update_attrs(9999, true, true), None);
        assert_eq!(idx.reparent(9999, 5), None);
        assert_eq!(idx.len(), 4);
        assert_eq!(idx.live_len(), 4);
        assert_eq!(idx.permutation(SortKey::Name), perm_before.as_slice());
        assert_eq!(idx.content_generation(), generation);
    }

    #[test]
    fn rename_dir_with_itself_as_parent_keeps_current_parent() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (top, sub) = (u16s("top"), u16s("sub"));
        b.push(raw(10, 5, &top, true, 0, 1));
        b.push(raw(20, 10, &sub, true, 0, 2));
        let mut idx = b.finish();
        let top_id = idx.entry_by_record(10).unwrap();
        let sub_id = idx.entry_by_record(20).unwrap();

        // new_parent_record == own record: the parent write is guarded, no
        // self-cycle is created and the path chain still terminates.
        let renamed = u16s("renamed");
        assert_eq!(idx.rename_dir_in_place(20, &renamed, 20), Some(sub_id));
        assert_eq!(idx.parent(sub_id), top_id);
        let mut p = Vec::new();
        idx.append_path(sub_id, &mut p);
        assert_eq!(p, b"C:\\top\\renamed");
        assert_perm_name_sorted(&idx);

        // Unknown new parent attaches to the root (current pinned behavior,
        // same as push_raw's orphan handling).
        let renamed2 = u16s("renamed2");
        assert_eq!(
            idx.rename_dir_in_place(20, &renamed2, 424_242),
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
        assert_eq!(idx.reparent(50, 50), Some(docs));
        assert_eq!(idx.parent(docs), before);
    }

    #[test]
    fn update_attrs_recomputes_excluded_from_own_and_inherited_bits() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
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
        idx.reparent(30, 10).unwrap();
        assert!(idx.is_excluded(f_id));
        idx.update_attrs(30, false, false).unwrap();
        assert!(idx.is_excluded(f_id));

        // Marking a dir hidden excludes the dir itself; existing children
        // keep their stale bit until the next rescan (pinned current
        // behavior — same accepted-limitation class as moves, see flags doc).
        let plain_id = idx.entry_by_record(20).unwrap();
        idx.update_attrs(20, true, false).unwrap();
        assert!(idx.is_excluded(plain_id));
        assert!(!idx.is_excluded(g_id));

        // New entries created under it inherit immediately.
        let h = u16s("h.txt");
        let first_new = idx.len() as u32;
        let h_id = idx.upsert(&raw_attr(50, 20, &h, false, false, false));
        idx.merge_new_into_permutations(first_new);
        assert!(idx.is_excluded(h_id));
    }

    #[test]
    fn no_op_batches_keep_content_generation_monotonic() {
        let mut idx = build_sample();
        let g0 = idx.content_generation();
        let s0 = idx.structural_generation();
        let perm_before = idx.permutation(SortKey::Name).to_vec();

        // Empty batch (e.g. a dir-rename-only USN batch): generation still
        // moves so derived caches invalidate, permutations stay put.
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_eq!(idx.content_generation(), g0 + 1);
        assert_eq!(idx.permutation(SortKey::Name), perm_before.as_slice());

        // Tombstone-only batch: ids stay in the permutations (flag-only).
        idx.delete(60);
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_eq!(idx.content_generation(), g0 + 2);
        assert_eq!(idx.permutation(SortKey::Name), perm_before.as_slice());

        // Individual mutations between batches never bump on their own.
        idx.update_stat(100, 1, 1).unwrap();
        idx.update_attrs(100, true, false).unwrap();
        assert_eq!(idx.content_generation(), g0 + 2);
        // Content batches never touch the structural generation.
        assert_eq!(idx.structural_generation(), s0);
    }
}
