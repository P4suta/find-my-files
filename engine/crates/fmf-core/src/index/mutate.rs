use crate::wtf8;

use super::core::SortColumns;
use super::{
    EncodedEntry, EntryId, NO_PARENT, RawEntry, SortKey, VolumeIndex, flags, merge_sorted_tail,
};

impl VolumeIndex {
    // ── Incremental mutation (USN batches; see module docs) ──────────────

    /// Pool bytes `id` owns that compaction could reclaim: the folded copy
    /// always, plus the original copy when one exists.
    fn owned_name_bytes(&self, id: EntryId) -> u64 {
        let len = self.name_len[id as usize] as u64;
        if self.orig_off[id as usize] == u32::MAX {
            len
        } else {
            len * 2
        }
    }

    /// Insert or replace an entry for `record`. Replacement (same record
    /// number) tombstones the old entry — this is how renames work.
    /// Returns the new id. Caller must finish the batch with
    /// [`Self::merge_new_into_permutations`].
    pub fn upsert(&mut self, e: &RawEntry) -> EntryId {
        if let Some(old) = self.entry_by_record(e.record) {
            self.flag[old as usize] |= flags::TOMBSTONE;
            self.tombstones += 1;
            self.dead_name_bytes += self.owned_name_bytes(old);
        }
        // Parents are already live on the USN path; unknown ones attach to
        // root (orphan records do occur in real MFTs).
        let parent = self.entry_by_record(e.parent_record).unwrap_or(Self::ROOT);
        self.push_raw(e, parent)
    }

    /// Tombstoning is the whole deletion: the FRN index never finds dead
    /// entries (liveness filter), so there is nothing to unmap.
    pub fn delete(&mut self, record: u64) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        self.flag[id as usize] |= flags::TOMBSTONE;
        self.tombstones += 1;
        self.dead_name_bytes += self.owned_name_bytes(id);
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
        if self.is_dir(id) {
            self.dir_topology_generation += 1; // descendant paths moved
        }
        Some(id)
    }

    /// Rename/move a *directory* in place. Directories must keep their
    /// `EntryId` stable — children's `parent` fields point at it — so instead
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

        // The old name bytes are abandoned where they live.
        self.dead_name_bytes += self.owned_name_bytes(id);
        let off = self.lower_pool.len();
        let mut orig = Vec::with_capacity(name_utf16.len() * 3);
        wtf8::push_wtf8_pair(name_utf16, &mut orig, &mut self.lower_pool);
        self.name_off[id as usize] = off as u32;
        self.name_len[id as usize] = (self.lower_pool.len() - off) as u16;
        self.orig_off[id as usize] = self.push_orig_if_differs(off, &orig);
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
        self.dir_topology_generation += 1; // descendant paths renamed
        Some(id)
    }

    /// Update size/mtime in place (`USN_REASON_DATA`_* without a name change).
    pub fn update_stat(&mut self, record: u64, size: u64, mtime: i64) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        self.set_size(id, size);
        self.mtime[id as usize] = mtime;
        Some(id)
    }

    /// Merge entries `first_new..len` (already appended, unsorted) into the
    /// name permutation (in place — see `merge_sorted_tail`), then bump
    /// the content generation. Call once per USN batch. The lazy size/mtime
    /// permutation caches catch up on their next sorted query (the
    /// generation bump is their invalidation signal).
    pub fn merge_new_into_permutations(&mut self, first_new: EntryId) {
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
        let mut batch: Vec<EntryId> = (first_new..self.len() as u32).collect();
        if !batch.is_empty() {
            // Split the borrow: the `&mut` permutation alongside the shared
            // key columns, comparing through the same SortColumns order
            // that built it.
            let Self { perm_name, .. } = self;
            let cols = SortColumns::new(
                &self.lower_pool,
                &self.name_off,
                &self.name_len,
                &self.size_lo,
                &self.size_ovf,
                &self.mtime,
            );
            batch.sort_unstable_by(|&a, &b| cols.cmp_by(SortKey::Name, a, b));
            merge_sorted_tail(perm_name, &batch, |a, b| cols.cmp_by(SortKey::Name, a, b));
        }
        self.content_generation += 1;
    }

    /// Store the original spelling only when it differs from the folded
    /// bytes just appended at `lower_off` — the fold-identical majority
    /// costs nothing beyond the sentinel (ADR-0004).
    fn push_orig_if_differs(&mut self, lower_off: usize, orig: &[u8]) -> u32 {
        if orig == &self.lower_pool[lower_off..] {
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

    /// Append with a pre-resolved parent: the USN path resolves against the
    /// live index (see [`Self::upsert`]); the initial-scan builder passes a
    /// provisional ROOT because `finish()` re-resolves every parent anyway —
    /// a per-push lookup against the unmerged FRN tail would be O(n²) there.
    pub(super) fn push_raw(&mut self, e: &RawEntry, parent: EntryId) -> EntryId {
        assert!(
            self.lower_pool.len() + e.name_utf16.len() * 4 < u32::MAX as usize,
            "name pool overflow"
        );
        let off = self.lower_pool.len();
        let mut orig = Vec::with_capacity(e.name_utf16.len() * 3);
        wtf8::push_wtf8_pair(e.name_utf16, &mut orig, &mut self.lower_pool);
        let orig_off = self.push_orig_if_differs(off, &orig);
        self.push_columns(
            off,
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
            self.lower_pool.len() + e.lower_wtf8.len() < u32::MAX as usize,
            "name pool overflow"
        );
        let off = self.lower_pool.len();
        self.lower_pool.extend_from_slice(e.lower_wtf8);
        let orig_off = self.push_orig_if_differs(off, e.name_wtf8);
        self.push_columns(
            off,
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

    /// Shared column append after the name bytes already landed in the pools
    /// at `off`/`orig_off`. The flag/parent logic must stay identical between
    /// the utf16 (`push_raw`) and pre-encoded (`push_encoded`) entry points.
    #[allow(clippy::too_many_arguments)]
    fn push_columns(
        &mut self,
        off: usize,
        orig_off: u32,
        parent: EntryId,
        frn: u64,
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
        self.name_off.push(off as u32);
        self.name_len.push((self.lower_pool.len() - off) as u16);
        self.orig_off.push(orig_off);
        self.parent.push(parent);
        self.push_size(size);
        self.mtime.push(mtime);
        self.frn.push(frn);
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

    /// Update raw attribute bits (USN `BASIC_INFO_CHANGE`) and the derived
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
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (alpha, mike, zulu, child) = (u16s("alpha"), u16s("mike"), u16s("zulu"), u16s("a.txt"));
        b.push(raw(10, 5, &alpha, true, 0, 1));
        b.push(raw(20, 5, &mike, true, 0, 2));
        b.push(raw(30, 5, &zulu, true, 0, 3));
        b.push(raw(11, 10, &child, false, 1, 4));
        let mut idx = b.finish();
        let dir = idx.entry_by_record(10).unwrap();

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
        // Name renames never touch sizes/mtimes, so the lazy size/mtime
        // orders (query::memo) stay valid without any signal from here.
    }

    #[test]
    fn mutations_on_unknown_records_are_safe_noops() {
        let mut idx = build_sample();
        let generation = idx.content_generation();
        let perm_before = idx.name_permutation().to_vec();
        let ghost = u16s("ghost");
        assert_eq!(idx.rename_dir_in_place(9999, &ghost, 5), None);
        assert_eq!(idx.delete(9999), None);
        assert_eq!(idx.update_stat(9999, 1, 1), None);
        assert_eq!(idx.update_attrs(9999, true, true), None);
        assert_eq!(idx.reparent(9999, 5), None);
        assert_eq!(idx.len(), 4);
        assert_eq!(idx.live_len(), 4);
        assert_eq!(idx.name_permutation(), perm_before.as_slice());
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
                        idx.upsert(&raw(
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
            idx.merge_new_into_permutations(first_new);

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
        let mut b = VolumeIndexBuilder::new("C:", 5);
        for (i, name) in cases.iter().enumerate() {
            let units = u16s(name);
            b.push(raw(100 + i as u64, 5, &units, false, 1, 1));
        }
        let mut idx = b.finish();
        // Lone surrogate through the USN write path.
        let first_new = idx.len() as u32;
        idx.upsert(&raw(900, 5, &[0x0041, 0xD800, 0x0042], false, 1, 1));
        idx.merge_new_into_permutations(first_new);

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

    /// In-place dir renames cross the fold-identity boundary in both
    /// directions: gaining an original copy and dropping back to the
    /// shared folded bytes.
    #[test]
    fn dir_rename_crosses_fold_identity_both_ways() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let plain = u16s("plain");
        b.push(raw(10, 5, &plain, true, 0, 1));
        let mut idx = b.finish();
        let id = idx.entry_by_record(10).unwrap();
        assert_eq!(idx.name(id), b"plain");
        assert_eq!(idx.lower_name(id), b"plain");

        let upper = u16s("Upper");
        idx.rename_dir_in_place(10, &upper, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_eq!(idx.name(id), b"Upper");
        assert_eq!(idx.lower_name(id), b"upper");

        let back = u16s("back_to_lower");
        idx.rename_dir_in_place(10, &back, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
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
        let id = idx.upsert(&raw(900, 50, &name, false, (6u64 << 30) + 7, 1));
        idx.merge_new_into_permutations(first_new);
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

    /// `dead_name_bytes` follows every pool-garbage source — the folded copy
    /// always, the original copy when one existed ("Note.TXT" does, the
    /// all-lowercase names don't); snapshot restore recomputes the
    /// tombstone share (rename gaps are a lost lower bound).
    #[test]
    fn dead_name_bytes_tracks_pool_garbage() {
        let owned = |idx: &VolumeIndex, record: u64| {
            let id = idx.entry_by_record(record).unwrap();
            let len = idx.name(id).len() as u64;
            if idx.name(id) == idx.lower_name(id) {
                len
            } else {
                len * 2
            }
        };
        let mut idx = build_sample();
        assert_eq!(idx.stats("C:").dead_name_bytes, 0);

        let note = owned(&idx, 100); // "Note.TXT": folded + orig copy
        assert_eq!(note, 16);
        let first_new = idx.len() as u32;
        let renamed = u16s("renamed.txt");
        idx.upsert(&raw(100, 50, &renamed, false, 1, 1));
        idx.merge_new_into_permutations(first_new);
        assert_eq!(idx.stats("C:").dead_name_bytes, note);

        let big = owned(&idx, 60); // "big.bin": folded copy only
        assert_eq!(big, 7);
        idx.delete(60);
        assert_eq!(idx.stats("C:").dead_name_bytes, note + big);

        let docs = owned(&idx, 50);
        let dir2 = u16s("docs2");
        idx.rename_dir_in_place(50, &dir2, 5);
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
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_eq!(idx.content_generation(), g0 + 1);
        assert_eq!(idx.name_permutation(), perm_before.as_slice());

        // Tombstone-only batch: ids stay in the permutations (flag-only).
        idx.delete(60);
        idx.merge_new_into_permutations(idx.len() as u32);
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
