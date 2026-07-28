//! Record-number → `EntryId` lookup as a sorted permutation (ADR-0005): one
//! id array maintained merge-only, exactly like the sort permutations.
//! Appends collect in an unmerged tail that lookups scan linearly, and the
//! end-of-batch merge folds them in sorted order. The keys are the `frn`
//! column itself, read through the id indirection — a probe pays one extra
//! cache miss, which only the USN path and the builder's parent resolution
//! ever do (never a search).
//!
//! Deletions never touch the array: a tombstoned entry simply fails the
//! liveness filter at lookup time. Several live ids may share one full FRN:
//! each row is one directory link, while the FRN identifies the underlying
//! NTFS object. At most one *generation* (full FRN sequence) is live for a
//! record number. Identity-sensitive USN callers therefore range over every
//! live row for the key and then compare the complete FRN.

use rayon::prelude::*;

use super::{EntryId, Frn, RecordNo, flags};

#[derive(Default)]
pub(super) struct FrnIndex {
    /// `EntryIds` ordered by (masked record number, id). Appended entries
    /// always carry the largest ids, so equal keys read old-before-new.
    ids: Vec<EntryId>,
    /// Entries `[0, covers)` are represented; later ids are found by the
    /// tail scan until [`Self::merge_appended`] runs (end of USN batch).
    covers: u32,
}

#[inline]
fn is_live(flag: &[u8], id: EntryId) -> bool {
    flag[id as usize] & flags::TOMBSTONE == 0
}

impl FrnIndex {
    /// Live ids in `(record number, EntryId)` order.
    pub(super) fn sorted_ids(&self) -> &[EntryId] {
        &self.ids
    }

    /// Build from scratch over every live entry (initial scan finish,
    /// snapshot restore).
    pub(super) fn build(frn: &[u64], flag: &[u8]) -> Self {
        let mut pairs: Vec<(RecordNo, EntryId)> = (0..frn.len() as u32)
            .filter(|&id| is_live(flag, id))
            .map(|id| (Frn(frn[id as usize]).record(), id))
            .collect();
        pairs.par_sort_unstable();
        Self {
            ids: pairs.iter().map(|p| p.1).collect(),
            covers: frn.len() as u32,
        }
    }

    /// One live entry for `key` (a record number), if any.
    ///
    /// This is the cheap representative lookup used for directory-parent
    /// resolution and compatibility call sites. A file may have several live
    /// link rows; object/link-sensitive mutations must use [`Self::lookup_all`].
    pub(super) fn lookup(&self, key: RecordNo, frn: &[u64], flag: &[u8]) -> Option<EntryId> {
        self.lookup_all(key, frn, flag).next()
    }

    /// Every live link row for `key`.
    ///
    /// The unmerged tail comes first (newest id first), followed by the
    /// key-equal range in the sorted body. The two ranges are disjoint at
    /// `covers`, so each id is yielded exactly once without allocating.
    pub(super) fn lookup_all<'a>(
        &'a self,
        key: RecordNo,
        frn: &'a [u64],
        flag: &'a [u8],
    ) -> impl Iterator<Item = EntryId> + 'a {
        let tail = (self.covers..frn.len() as u32)
            .rev()
            .filter(move |&id| Frn(frn[id as usize]).record() == key && is_live(flag, id));
        let key_of = |id: EntryId| Frn(frn[id as usize]).record();
        let start = self.ids.partition_point(|&id| key_of(id) < key);
        let end = self.ids[start..].partition_point(|&id| key_of(id) == key) + start;
        let body = self.ids[start..end]
            .iter()
            .copied()
            .filter(move |&id| is_live(flag, id));
        tail.chain(body)
    }

    /// Validate object grouping: every live row sharing a record number must
    /// share its full sequence and kind, and directories must have one row.
    /// Multiple same-generation file rows are the supported hard-link case.
    pub(super) fn has_valid_live_object_groups(&self, frn: &[u64], flag: &[u8]) -> bool {
        let mut previous: Option<(RecordNo, u64, bool)> = None;
        for &id in &self.ids {
            if !is_live(flag, id) {
                continue;
            }
            let full = frn[id as usize];
            let record = Frn(full).record();
            let is_dir = flag[id as usize] & flags::IS_DIR != 0;
            if let Some((previous_record, previous_full, previous_is_dir)) = previous
                && previous_record == record
                && (previous_full != full || previous_is_dir != is_dir || is_dir)
            {
                return false;
            }
            previous = Some((record, full, is_dir));
        }
        true
    }

    /// Fold the appended entries into sorted order — live ones only;
    /// anything tombstoned before its first merge can never be looked up
    /// again. In place: each batch pair binary-searches its insertion point
    /// and the segments between insertion points move once (`copy_within`)
    /// — O(batch·log n) comparisons, no reallocation (ADR-0008).
    /// Equal keys keep old-before-new order; liveness and link enumeration do
    /// not depend on the order within one record's range.
    pub(super) fn merge_appended(&mut self, frn: &[u64], flag: &[u8]) {
        let n = frn.len() as u32;
        let mut batch: Vec<(RecordNo, EntryId)> = (self.covers..n)
            .filter(|&id| is_live(flag, id))
            .map(|id| (Frn(frn[id as usize]).record(), id))
            .collect();
        self.covers = n;
        if batch.is_empty() {
            return;
        }
        batch.sort_unstable();

        let old = self.ids.len();
        super::reserve_bounded(&mut self.ids, batch.len());
        self.ids.resize(old + batch.len(), 0);
        let key_of = |id: EntryId| Frn(frn[id as usize]).record();
        let mut hi = old; // unmoved prefix of the old array (exclusive end)
        let mut k = old + batch.len(); // write cursor (exclusive end)
        for j in (0..batch.len()).rev() {
            let (key, id) = batch[j];
            let pos = self.ids[..hi].partition_point(|&oid| key_of(oid) <= key);
            let seg = hi - pos;
            self.ids.copy_within(pos..hi, k - seg);
            k -= seg + 1;
            self.ids[k] = id;
            hi = pos;
        }
        debug_assert_eq!(k, hi, "merge cursors must close");
    }

    /// Remapped copy for compaction: dead ids drop out, survivors renumber.
    /// Keys (masked record numbers) are copied unchanged by the caller and
    /// the remap preserves relative id order, so the (key, id) order — and
    /// with it lookup's binary search — survives without a re-sort.
    pub(super) fn compact(&self, remap: &[EntryId], new_len: u32) -> Self {
        debug_assert_eq!(
            self.covers as usize,
            remap.len(),
            "compact at a batch boundary only (unmerged tail would be lost)"
        );
        Self {
            ids: self
                .ids
                .iter()
                .filter_map(|&id| match remap[id as usize] {
                    super::NO_PARENT => None,
                    new_id => Some(new_id),
                })
                .collect(),
            covers: new_len,
        }
    }

    pub(super) const fn bytes(&self) -> u64 {
        (self.ids.capacity() * 4) as u64
    }

    pub(super) fn shrink_to_fit(&mut self) {
        self.ids.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::EntryId;
    use crate::index::testutil::{build_hardlink_sample, build_sample, raw, u16s};
    use crate::index::{Frn, RecordNo};

    /// Reference implementation (forward full merge): equal keys take the
    /// old pair first.
    fn forward_merge_reference(
        keys: &mut Vec<RecordNo>,
        ids: &mut Vec<EntryId>,
        batch: &[(RecordNo, EntryId)],
    ) {
        let mut nk = Vec::with_capacity(keys.len() + batch.len());
        let mut ni = Vec::with_capacity(ids.len() + batch.len());
        let (mut i, mut j) = (0, 0);
        while i < keys.len() && j < batch.len() {
            if keys[i] <= batch[j].0 {
                nk.push(keys[i]);
                ni.push(ids[i]);
                i += 1;
            } else {
                nk.push(batch[j].0);
                ni.push(batch[j].1);
                j += 1;
            }
        }
        nk.extend_from_slice(&keys[i..]);
        ni.extend_from_slice(&ids[i..]);
        for &(k, id) in &batch[j..] {
            nk.push(k);
            ni.push(id);
        }
        *keys = nk;
        *ids = ni;
    }

    /// Random rename/delete storms: the in-place merge must produce the
    /// byte-identical id array the forward reference does, batch after
    /// batch (record keys are never mutated in place, so both orders stay
    /// truly sorted and the sorted stable merge is unique). The reference
    /// carries explicit key/id pairs — the production array dropped the
    /// key copy and reads keys through the id indirection, so the derived
    /// key sequence is asserted too.
    #[test]
    fn in_place_merge_is_byte_identical_to_the_forward_reference() {
        let mut idx = build_sample();
        let mut ref_ids = idx.frn_index.ids.clone();
        let mut ref_keys: Vec<RecordNo> = ref_ids.iter().map(|&id| idx.frn(id).record()).collect();
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..100 {
            let first_new = idx.len() as u32;
            for _ in 0..=(rng() % 8) {
                let record = 100 + rng() % 40;
                if rng() % 3 < 2 {
                    let name = u16s(&format!("f{}.txt", rng() % 1000));
                    idx.upsert_synthetic(&raw(record, 50, &name, false, 1, 1));
                } else {
                    idx.delete(record);
                }
            }
            // Mirror merge_appended's batch input exactly (live tail only).
            let mut batch: Vec<(RecordNo, EntryId)> = (first_new..idx.len() as u32)
                .filter(|&id| idx.is_live(id))
                .map(|id| (idx.frn(id).record(), id))
                .collect();
            batch.sort_unstable();
            forward_merge_reference(&mut ref_keys, &mut ref_ids, &batch);

            idx.merge_new_into_permutations(first_new)
                .expect("fixture topology remains valid");
            assert_eq!(idx.frn_index.ids, ref_ids);
            let derived_keys: Vec<RecordNo> = idx
                .frn_index
                .ids
                .iter()
                .map(|&id| idx.frn(id).record())
                .collect();
            assert_eq!(derived_keys, ref_keys);
        }
    }

    /// The latest upsert of a record must win — found via the unmerged
    /// tail during the batch and via the sorted arrays after the merge,
    /// with identical results.
    #[test]
    fn rename_storm_resolves_to_the_latest_before_and_after_merge() {
        let mut idx = build_sample();
        let live_before = idx.live_len();
        let first_new = idx.len() as u32;
        let mut last = None;
        for i in 0..5u64 {
            let name = u16s(&format!("storm_{i}.txt"));
            last = Some(idx.upsert_synthetic(&raw(100, 50, &name, false, 1, i as i64)));
        }
        let tail_hit = idx.entry_by_record(100).unwrap();
        assert_eq!(Some(tail_hit), last, "tail scan must see the newest");
        assert_eq!(idx.name(tail_hit), b"storm_4.txt");

        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(idx.entry_by_record(100), last, "sorted lookup agrees");
        assert_eq!(idx.live_len(), live_before, "storm nets zero live change");
    }

    /// NTFS reuses record numbers: a delete followed by a create of the
    /// same record must resolve to the new entry, never the tombstone.
    #[test]
    fn record_reuse_after_delete_resolves_to_the_new_entry() {
        let mut idx = build_sample();
        idx.delete(60).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert_eq!(idx.entry_by_record(60), None, "deleted record misses");

        let first_new = idx.len() as u32;
        let name = u16s("reborn.txt");
        let id = idx.upsert_synthetic(&raw(60, 50, &name, false, 7, 7));
        assert_eq!(idx.entry_by_record(60), Some(id), "tail finds the rebirth");
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(idx.entry_by_record(60), Some(id), "merge keeps it");
        assert_eq!(idx.name(id), b"reborn.txt");
    }

    /// An entry created and deleted within one batch never surfaces, and
    /// the merge does not resurrect it.
    #[test]
    fn create_then_delete_within_a_batch_stays_gone() {
        let mut idx = build_sample();
        let first_new = idx.len() as u32;
        let name = u16s("flash.tmp");
        idx.upsert_synthetic(&raw(777, 50, &name, false, 1, 1));
        assert!(idx.entry_by_record(777).is_some());
        idx.delete(777).unwrap();
        assert_eq!(idx.entry_by_record(777), None, "gone in the tail");
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        assert_eq!(idx.entry_by_record(777), None, "gone after the merge");
    }

    #[test]
    fn duplicate_frn_range_preserves_every_hard_link_before_and_after_merge() {
        let mut idx = build_hardlink_sample();
        let object = Frn((1u64 << 48) | 0x64);
        let mut names: Vec<_> = idx
            .entries_by_frn(object)
            .map(|id| idx.name(id).to_vec())
            .collect();
        names.sort();
        assert_eq!(names, [b"alias.txt".to_vec(), b"shared.txt".to_vec()]);

        let third = u16s("third.txt");
        let entry = raw(100, 10, &third, false, 42, 3);
        let first_new = idx.len() as u32;
        idx.upsert_link_usn(&entry)
            .expect("fixture parent is exact");
        assert_eq!(
            idx.entries_by_frn(object).count(),
            3,
            "the unmerged tail participates in the same FRN range"
        );

        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        let mut names: Vec<_> = idx
            .entries_by_frn(object)
            .map(|id| idx.name(id).to_vec())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                b"alias.txt".to_vec(),
                b"shared.txt".to_vec(),
                b"third.txt".to_vec()
            ]
        );
    }
}
