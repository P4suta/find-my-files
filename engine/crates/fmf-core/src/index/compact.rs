//! Compaction (M2): rebuild the index without tombstoned rows and without
//! the name bytes renames abandoned in the pools. Without it both grow
//! forever under USN traffic and eventually eat the B/entry RAM budget.
//!
//! The whole trick is the remapping order: live entries keep their relative
//! id order (old-id ascending → new ids 0..live). Every sorted structure
//! orders by (key, id) with identical keys on both sides, so filtering the
//! dead and renumbering the survivors preserves sortedness — `perm_name`
//! and the FRN index copy over in O(n) with **no re-sort** (ADR-0009).
//!
//! Swap-in goes through `VolumeSlot::install_index`, which bumps the
//! structural generation: open result handles go hard-stale and the UI
//! re-issues its query (docs/ARCHITECTURE.md, generation 2層).

use parking_lot::Mutex;

use super::{EntryId, NO_PARENT, VolumeIndex};

/// Below this size the garbage can't be worth a rebuild.
const COMPACT_MIN_ENTRIES: usize = 100_000;
/// Tombstone share that triggers compaction (matches the `OffsetTable`'s
/// stale-rebuild instinct: past ~1/8 dead weight, rebuilding wins).
const COMPACT_TOMBSTONE_RATIO: f64 = 0.125;
/// Reclaimable pool bytes that trigger compaction regardless of ratio.
const COMPACT_DEAD_NAME_BYTES: u64 = 32 << 20;

impl VolumeIndex {
    /// Should this index be compacted? (Policy entry point for the volume
    /// thread, once per applied USN batch.)
    pub fn compaction_due(&self) -> bool {
        self.compaction_due_past(COMPACT_MIN_ENTRIES)
    }

    fn compaction_due_past(&self, min_entries: usize) -> bool {
        self.len() >= min_entries.max(1)
            && (self.tombstone_ratio() > COMPACT_TOMBSTONE_RATIO
                || self.dead_name_bytes > COMPACT_DEAD_NAME_BYTES)
    }

    /// A compacted copy: live entries only, pools rebuilt without garbage,
    /// permutation and FRN index remapped without re-sorting. Children of a
    /// tombstoned directory attach to the root — the same orphan policy as
    /// `push_raw`. The copy starts at generation zero on all three counters;
    /// `install_index` carries the structural generation forward.
    ///
    /// Call only at a batch boundary (the FRN index must cover every entry —
    /// `merge_new_into_permutations` just ran).
    #[must_use]
    pub fn compacted(&self) -> Self {
        let n = self.len();
        // Old → new id; NO_PARENT marks the dead.
        let mut remap: Vec<EntryId> = vec![NO_PARENT; n];
        let mut live: u32 = 0;
        for id in 0..n as u32 {
            if self.is_live(id) {
                remap[id as usize] = live;
                live += 1;
            }
        }
        debug_assert!(
            self.is_live(Self::ROOT),
            "the root entry is never tombstoned"
        );

        let mut out = Self {
            lower_pool: Vec::with_capacity(self.lower_pool.len()),
            orig_pool: Vec::with_capacity(self.orig_pool.len()),
            orig_off: Vec::with_capacity(live as usize),
            name_off: Vec::with_capacity(live as usize),
            name_len: Vec::with_capacity(live as usize),
            parent: Vec::with_capacity(live as usize),
            size_lo: Vec::with_capacity(live as usize),
            size_ovf: rustc_hash::FxHashMap::default(),
            mtime: Vec::with_capacity(live as usize),
            frn: Vec::with_capacity(live as usize),
            flag: Vec::with_capacity(live as usize),
            frn_index: self.frn_index.compact(&remap, live),
            perm_name: Vec::with_capacity(live as usize),
            content_generation: 0,
            structural_generation: 0,
            dir_topology_generation: 0,
            tombstones: 0,
            dead_name_bytes: 0,
            derived_cache: Mutex::new(None),
        };

        for id in 0..n as u32 {
            if !self.is_live(id) {
                continue;
            }
            out.name_off.push(out.lower_pool.len() as u32);
            out.name_len.push(self.name_len[id as usize]);
            out.lower_pool.extend_from_slice(self.lower_name(id));
            out.orig_off.push(if self.is_fold_identical(id) {
                u32::MAX
            } else {
                let off = out.orig_pool.len() as u32;
                out.orig_pool.extend_from_slice(self.name(id));
                off
            });
            let p = self.parent[id as usize];
            out.parent.push(if p == NO_PARENT {
                NO_PARENT // the root
            } else {
                match remap[p as usize] {
                    NO_PARENT => Self::ROOT, // orphaned by a dead dir
                    new_p => new_p,
                }
            });
            out.push_size(self.size(id));
            out.mtime.push(self.mtime[id as usize]);
            out.frn.push(self.frn[id as usize]);
            out.flag.push(self.flag[id as usize]);
        }

        out.perm_name = self
            .perm_name
            .iter()
            .filter_map(|&id| match remap[id as usize] {
                NO_PARENT => None,
                new_id => Some(new_id),
            })
            .collect();

        out.shrink_to_fit();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SortKey;
    use crate::index::testutil::{build_sample, raw, u16s};

    /// Garbage from renames + deletes, then compact: every live record
    /// resolves identically, paths and names byte-match, sorted structures
    /// hold without re-sorting, counters reset.
    #[test]
    fn compaction_drops_garbage_and_preserves_live_entries() {
        let mut idx = build_sample();
        // Rename storm on 100 (tombstone churn + pool garbage), one delete,
        // one in-place dir rename (pool garbage without a tombstone), one
        // ≥4GiB file (size-overflow remap), one cased name (orig pool).
        for i in 0..4u64 {
            let first_new = idx.len() as u32;
            let name = u16s(&format!("storm_{i}.TXT"));
            idx.upsert(&raw(100, 50, &name, false, i, i as i64));
            idx.merge_new_into_permutations(first_new);
        }
        let first_new = idx.len() as u32;
        let huge = u16s("Huge.ISO");
        idx.upsert(&raw(700, 50, &huge, false, (7u64 << 30) + 5, 9));
        idx.merge_new_into_permutations(first_new);
        idx.delete(60);
        let dir2 = u16s("docs_v2");
        idx.rename_dir_in_place(50, &dir2, 5);
        idx.merge_new_into_permutations(idx.len() as u32);

        let live_before = idx.live_len();
        let expect: Vec<(u64, Vec<u8>, Vec<u8>, u64)> = [5u64, 50, 100, 700]
            .iter()
            .map(|&rec| {
                let id = idx.entry_by_record(rec).unwrap();
                let mut p = Vec::new();
                idx.append_path(id, &mut p);
                (rec, idx.name(id).to_vec(), p, idx.size(id))
            })
            .collect();

        let c = idx.compacted();
        assert_eq!(c.len(), live_before);
        assert_eq!(c.live_len(), live_before);
        // After compaction tombstones == 0, so the ratio is exactly 0.0.
        #[expect(clippy::float_cmp, reason = "0 tombstones yields an exact 0.0 ratio")]
        {
            assert_eq!(c.tombstone_ratio(), 0.0);
        }
        assert_eq!(c.stats("C:").dead_name_bytes, 0);
        // Pools shrank: the storm's abandoned bytes are gone.
        assert!(c.stats("C:").lower_pool_bytes < idx.stats("C:").lower_pool_bytes);

        for (rec, name, path, size) in &expect {
            let id = c.entry_by_record(*rec).unwrap_or_else(|| {
                panic!("record {rec} lost in compaction");
            });
            assert_eq!(c.name(id), &name[..], "record {rec}");
            let mut p = Vec::new();
            c.append_path(id, &mut p);
            assert_eq!(&p, path, "record {rec}");
            assert_eq!(c.size(id), *size, "record {rec}");
        }
        assert_eq!(c.entry_by_record(60), None, "deleted record stays gone");

        // perm_name is a strictly sorted complete permutation — without
        // having been re-sorted.
        let perm = c.name_permutation();
        assert_eq!(perm.len(), c.len());
        let mut seen: Vec<EntryId> = perm.to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..c.len() as u32).collect::<Vec<_>>());
        for w in perm.windows(2) {
            assert!(c.cmp_by(SortKey::Name, w[0], w[1]).is_lt());
        }

        // Round-trips through a snapshot like any other index.
        let mut buf = Vec::new();
        c.write_snapshot(&mut buf, 1, 2).unwrap();
        let (loaded, _, _) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
        assert_eq!(loaded.len(), c.len());
    }

    /// Children of a tombstoned directory attach to the root (`push_raw`'s
    /// orphan policy) instead of dangling.
    #[test]
    fn compaction_reattaches_orphans_of_dead_dirs() {
        let mut idx = build_sample();
        idx.delete(50); // "docs", parent of record 100
        idx.merge_new_into_permutations(idx.len() as u32);
        let c = idx.compacted();
        let note = c.entry_by_record(100).unwrap();
        assert_eq!(c.parent(note), VolumeIndex::ROOT);
        let mut p = Vec::new();
        c.append_path(note, &mut p);
        assert_eq!(p, b"C:\\Note.TXT");
    }

    #[test]
    fn compaction_policy_thresholds() {
        let mut idx = build_sample();
        assert!(
            !idx.compaction_due_past(1),
            "clean index must not trigger on garbage thresholds"
        );
        idx.delete(60);
        // 1 of 4 entries dead = 25% > 12.5%.
        assert!(idx.compaction_due_past(1));
        assert!(
            !idx.compaction_due(),
            "tiny volumes never trigger (min-entries floor)"
        );
    }
}
