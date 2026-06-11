//! Record-number → EntryId lookup as a sorted permutation.
//!
//! Replaces the FxHashMap (~25 B/entry once capacity padding is counted —
//! the single largest RAM line after the name pools) with two parallel
//! arrays (12 B/entry) maintained merge-only, exactly like the sort
//! permutations: appends collect in an unmerged tail that lookups scan
//! linearly, and the end-of-batch merge folds them in sorted order.
//!
//! Deletions never touch the arrays: a tombstoned entry simply fails the
//! liveness filter at lookup time. Renames (tombstone + append, same
//! record) and NTFS record reuse therefore leave several pairs per key,
//! of which at most one is live — the invariant the mutation API upholds.

use rayon::prelude::*;

use super::{EntryId, flags, masked};

#[derive(Default)]
pub(super) struct FrnIndex {
    /// Masked record numbers, ascending (duplicates possible — see above).
    keys: Vec<u64>,
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
    /// Build from scratch over every live entry (initial scan finish,
    /// snapshot restore).
    pub(super) fn build(frn: &[u64], flag: &[u8]) -> Self {
        let mut pairs: Vec<(u64, EntryId)> = (0..frn.len() as u32)
            .filter(|&id| is_live(flag, id))
            .map(|id| (masked(frn[id as usize]), id))
            .collect();
        pairs.par_sort_unstable();
        FrnIndex {
            keys: pairs.iter().map(|p| p.0).collect(),
            ids: pairs.iter().map(|p| p.1).collect(),
            covers: frn.len() as u32,
        }
    }

    /// The live entry for `key` (a masked record number), if any.
    pub(super) fn lookup(&self, key: u64, frn: &[u64], flag: &[u8]) -> Option<EntryId> {
        // Unmerged tail first, newest first: within a batch the latest
        // upsert for a record is the live one.
        for id in (self.covers..frn.len() as u32).rev() {
            if masked(frn[id as usize]) == key && is_live(flag, id) {
                return Some(id);
            }
        }
        let start = self.keys.partition_point(|&k| k < key);
        for i in start..self.keys.len() {
            if self.keys[i] != key {
                break;
            }
            if is_live(flag, self.ids[i]) {
                return Some(self.ids[i]);
            }
        }
        None
    }

    /// Fold the appended entries into sorted order — live ones only;
    /// anything tombstoned before its first merge can never be looked up
    /// again. Two-pointer merge, O(existing + batch log batch).
    pub(super) fn merge_appended(&mut self, frn: &[u64], flag: &[u8]) {
        let n = frn.len() as u32;
        let mut batch: Vec<(u64, EntryId)> = (self.covers..n)
            .filter(|&id| is_live(flag, id))
            .map(|id| (masked(frn[id as usize]), id))
            .collect();
        self.covers = n;
        if batch.is_empty() {
            return;
        }
        batch.sort_unstable();

        let mut keys = Vec::with_capacity(self.keys.len() + batch.len());
        let mut ids = Vec::with_capacity(self.ids.len() + batch.len());
        let (mut i, mut j) = (0, 0);
        while i < self.keys.len() && j < batch.len() {
            if self.keys[i] <= batch[j].0 {
                keys.push(self.keys[i]);
                ids.push(self.ids[i]);
                i += 1;
            } else {
                keys.push(batch[j].0);
                ids.push(batch[j].1);
                j += 1;
            }
        }
        keys.extend_from_slice(&self.keys[i..]);
        ids.extend_from_slice(&self.ids[i..]);
        for &(k, id) in &batch[j..] {
            keys.push(k);
            ids.push(id);
        }
        self.keys = keys;
        self.ids = ids;
    }

    pub(super) fn bytes(&self) -> u64 {
        (self.keys.capacity() * 8 + self.ids.capacity() * 4) as u64
    }

    pub(super) fn shrink_to_fit(&mut self) {
        self.keys.shrink_to_fit();
        self.ids.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use crate::index::testutil::{build_sample, raw, u16s};

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
            last = Some(idx.upsert(&raw(100, 50, &name, false, 1, i as i64)));
        }
        let tail_hit = idx.entry_by_record(100).unwrap();
        assert_eq!(Some(tail_hit), last, "tail scan must see the newest");
        assert_eq!(idx.name(tail_hit), b"storm_4.txt");

        idx.merge_new_into_permutations(first_new);
        assert_eq!(idx.entry_by_record(100), last, "sorted lookup agrees");
        assert_eq!(idx.live_len(), live_before, "storm nets zero live change");
    }

    /// NTFS reuses record numbers: a delete followed by a create of the
    /// same record must resolve to the new entry, never the tombstone.
    #[test]
    fn record_reuse_after_delete_resolves_to_the_new_entry() {
        let mut idx = build_sample();
        idx.delete(60).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_eq!(idx.entry_by_record(60), None, "deleted record misses");

        let first_new = idx.len() as u32;
        let name = u16s("reborn.txt");
        let id = idx.upsert(&raw(60, 50, &name, false, 7, 7));
        assert_eq!(idx.entry_by_record(60), Some(id), "tail finds the rebirth");
        idx.merge_new_into_permutations(first_new);
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
        idx.upsert(&raw(777, 50, &name, false, 1, 1));
        assert!(idx.entry_by_record(777).is_some());
        idx.delete(777).unwrap();
        assert_eq!(idx.entry_by_record(777), None, "gone in the tail");
        idx.merge_new_into_permutations(first_new);
        assert_eq!(idx.entry_by_record(777), None, "gone after the merge");
    }
}
