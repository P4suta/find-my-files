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
//! structural generation: renumbering makes every held `EntryId` meaningless,
//! so open result handles go hard stale and the client re-issues its query.

use parking_lot::Mutex;

use super::{EntryId, NO_PARENT, VolumeIndex};

/// Below this size sparse dead rows alone are not worth a rebuild. Absolute
/// pool garbage and dictionary churn bypass this floor: both otherwise grow
/// without bound even on permanently small volumes.
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
        let sparse_row_capacity_due =
            self.len() >= min_entries.max(1) && self.tombstone_ratio() > COMPACT_TOMBSTONE_RATIO;
        let absolute_pool_garbage_due = self.dead_name_bytes > COMPACT_DEAD_NAME_BYTES;
        let dictionary_churn_due = self.dict_appends_since_dedup as usize > self.live_len() / 4;
        sparse_row_capacity_due || absolute_pool_garbage_due || dictionary_churn_due
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
            dict_pool: Vec::with_capacity(self.dict_pool.len()),
            dict_off: Vec::with_capacity(live as usize),
            name_id: Vec::with_capacity(live as usize),
            orig_pool: Vec::with_capacity(self.orig_pool.len()),
            orig_off: Vec::with_capacity(live as usize),
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
            stat_generation: 0,
            dir_topology_generation: 0,
            exclusion_tree_dirty: false,
            tombstones: 0,
            dead_name_bytes: 0,
            dict_appends_since_dedup: 0,
            derived_cache: Mutex::new(None),
        };

        for id in 0..n as u32 {
            if !self.is_live(id) {
                continue;
            }
            let name_id = out.dict_off.len() as u32;
            let off = out.dict_pool.len();
            out.dict_pool.extend_from_slice(self.lower_name(id));
            out.dict_off.push(off as u32);
            out.name_id.push(name_id);
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

        // Collapse the per-entry dict appends into distinct names (ADR-0032),
        // and the per-entry originals into distinct copies (ADR-0033 Lever 1);
        // names are unchanged, so the just-remapped perm_name stays sorted.
        out.dedup_dict();
        out.dedup_orig();
        // Reattaching children of a dead directory to ROOT changes inherited
        // visibility just like a USN move; rebuild it while compaction is
        // already doing an O(n) pass.
        out.recompute_all_excluded();
        out.shrink_to_fit();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SortKey;
    use crate::index::testutil::{build_hardlink_sample, build_sample, raw, u16s};

    #[test]
    fn compaction_preserves_distinct_paths_that_share_one_frn() {
        let idx = build_hardlink_sample();
        let object = crate::index::Frn((1u64 << 48) | 0x64);
        let mut before: Vec<Vec<u8>> = idx
            .entries_by_frn(object)
            .map(|id| {
                let mut path = Vec::new();
                idx.append_path(id, &mut path).unwrap();
                path
            })
            .collect();
        before.sort();

        let compacted = idx.compacted();
        let mut after: Vec<Vec<u8>> = compacted
            .entries_by_frn(object)
            .map(|id| {
                let mut path = Vec::new();
                compacted.append_path(id, &mut path).unwrap();
                path
            })
            .collect();
        after.sort();

        assert_eq!(after, before);
        assert_eq!(
            after,
            [b"C:\\a\\shared.txt".to_vec(), b"C:\\b\\alias.txt".to_vec()]
        );
    }

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
            idx.upsert_synthetic(&raw(100, 50, &name, false, i, i as i64));
            idx.merge_new_into_permutations(first_new)
                .expect("fixture topology remains valid");
        }
        let first_new = idx.len() as u32;
        let huge = u16s("Huge.ISO");
        idx.upsert_synthetic(&raw(700, 50, &huge, false, (7u64 << 30) + 5, 9));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        idx.delete(60);
        let dir2 = u16s("docs_v2");
        idx.rename_dir_synthetic_in_place(50, &dir2, 5);
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        let live_before = idx.live_len();
        let expect: Vec<(u64, Vec<u8>, Vec<u8>, u64)> = [5u64, 50, 100, 700]
            .iter()
            .map(|&rec| {
                let id = idx.entry_by_record(rec).unwrap();
                let mut p = Vec::new();
                idx.append_path(id, &mut p).unwrap();
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
            c.append_path(id, &mut p).unwrap();
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
        let note_before = idx.entry_by_record(100).unwrap();
        idx.update_attrs(50, true, false).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        assert!(idx.is_excluded(note_before));
        idx.delete(50); // "docs", parent of record 100
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
        let c = idx.compacted();
        let note = c.entry_by_record(100).unwrap();
        assert_eq!(c.parent(note), VolumeIndex::ROOT);
        assert!(
            !c.is_excluded(note),
            "root reattachment clears dead-parent inheritance"
        );
        let mut p = Vec::new();
        c.append_path(note, &mut p).unwrap();
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
            "a tiny volume with only one cheap tombstone stays below the row-capacity floor"
        );
    }

    #[test]
    fn small_volume_directory_rename_churn_stays_bounded() {
        let mut idx = build_sample();
        let live = idx.live_len();
        let mut compactions = 0;
        let mut max_pool_bytes = 0;

        // This volume never approaches 100k rows. Production's per-batch
        // policy still has to dedup abandoned directory names, or a long-lived
        // removable/small volume leaks forever.
        for i in 0..256 {
            let renamed = u16s(&format!("docs_{i:03}"));
            idx.rename_dir_synthetic_in_place(50, &renamed, 5).unwrap();
            idx.merge_new_into_permutations(idx.len() as u32)
                .expect("fixture topology remains valid");
            max_pool_bytes = max_pool_bytes.max(idx.stats("C:").lower_pool_bytes);
            if idx.compaction_due() {
                idx = idx.compacted();
                compactions += 1;
            }
        }

        assert!(compactions > 0, "small-volume dict churn must trigger");
        assert_eq!(idx.len(), live);
        assert_eq!(idx.live_len(), live);
        assert!(
            idx.dict_appends_since_dedup as usize <= idx.live_len() / 4,
            "the production trigger keeps post-dedup churn bounded"
        );
        assert!(
            max_pool_bytes < 1024,
            "the pool stayed bounded instead of growing with all 256 renames"
        );
    }

    #[test]
    fn absolute_pool_garbage_bypasses_small_volume_row_floor() {
        let mut idx = build_sample();
        idx.dead_name_bytes = COMPACT_DEAD_NAME_BYTES + 1;
        assert!(idx.compaction_due());
    }
}

#[cfg(test)]
mod proptests {
    //! Equivalence property: `compacted()` is observably transparent. After an
    //! arbitrary mix of upserts (pool garbage) and deletes (tombstones), the
    //! compacted copy has exactly the same set of live records — same name,
    //! same path, same size — with the dead ones gone and zero tombstones.
    //! Each case is non-trivial by construction: record 10 always survives and
    //! record 11 is always deleted, so neither the "nothing dead" nor the
    //! "everything dead" degenerate case can make the property vacuous.

    use proptest::prelude::*;

    use crate::index::VolumeIndexBuilder;
    use crate::index::testutil::{raw, u16s};

    const NAMES: &[&str] = &["file", "DOC", "report.rs", "日本.txt", "x", "Note"];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn compaction_preserves_every_live_record_observably(
            names in proptest::collection::vec(0usize..NAMES.len(), 2..10),
            delete_extra in proptest::collection::vec(any::<bool>(), 2..10),
            rename_one in any::<bool>(),
        ) {
            // Records 10.. under the root; build a clean index first.
            let n = names.len();
            let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
            for (i, &name_idx) in names.iter().enumerate() {
                let nm = u16s(&format!("{}_{i}", NAMES[name_idx]));
                b.push(raw(10 + i as u64, 5, &nm, false, (i as u64) * 1000, i as i64));
            }
            let mut idx = b.finish();

            // Force the non-trivial frame: keep record 10, kill record 11.
            let mut dead: Vec<u64> = vec![11];
            for (i, &d) in delete_extra.iter().enumerate().take(n) {
                let rec = 10 + i as u64;
                if d && rec != 10 && rec != 11 {
                    dead.push(rec);
                }
            }

            // Optional rename of the kept record → pool garbage without a
            // tombstone, so compaction's pool rebuild is exercised too.
            if rename_one {
                let first_new = idx.len() as u32;
                idx.upsert_synthetic(&raw(10, 5, &u16s("renamed_kept"), false, 4242, 7));
                idx.merge_new_into_permutations(first_new).expect("fixture topology remains valid");
            }
            for &rec in &dead {
                idx.delete(rec);
            }
            idx.merge_new_into_permutations(idx.len() as u32).expect("fixture topology remains valid");
            // Live records are the volume root (record 5, seeded by the
            // builder) plus the known set 10..10+n minus the deleted ones —
            // capture each one's observable (name, path, size) by record.
            let mut live_recs: Vec<u64> = vec![5];
            live_recs.extend((0..n as u64).map(|i| 10 + i).filter(|r| !dead.contains(r)));
            let live: Vec<(u64, Vec<u8>, Vec<u8>, u64)> = live_recs
                .iter()
                .map(|&rec| {
                    let id = idx.entry_by_record(rec).expect("live record present pre-compaction");
                    let mut p = Vec::new();
                    idx.append_path(id, &mut p).unwrap();
                    (rec, idx.name(id).to_vec(), p, idx.size(id))
                })
                .collect();

            // Each record is a distinct FRN, so live_len must equal the known
            // survivor count — guards the property from a silent miscount.
            prop_assert_eq!(idx.live_len(), live_recs.len());

            let c = idx.compacted();

            prop_assert_eq!(c.len(), live_recs.len(), "compaction drops every tombstone");
            prop_assert_eq!(c.live_len(), live_recs.len());
            for (rec, name, path, size) in &live {
                let id = c
                    .entry_by_record(*rec)
                    .unwrap_or_else(|| panic!("live record {rec} lost in compaction"));
                prop_assert_eq!(c.name(id), &name[..], "name for record {}", rec);
                let mut p = Vec::new();
                c.append_path(id, &mut p).unwrap();
                prop_assert_eq!(&p, path, "path for record {}", rec);
                prop_assert_eq!(c.size(id), *size, "size for record {}", rec);
            }
            for &rec in &dead {
                prop_assert!(c.entry_by_record(rec).is_none(), "deleted {} resurfaced", rec);
            }
        }
    }
}
