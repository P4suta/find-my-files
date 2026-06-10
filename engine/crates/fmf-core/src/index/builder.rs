use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use super::{EntryId, NO_PARENT, RawEntry, SortKey, VolumeIndex, masked};

/// Two-pass builder for the initial scan: collect everything, then resolve
/// parents and sort the permutations (scan order ≠ parent-before-child).
pub struct VolumeIndexBuilder {
    idx: VolumeIndex,
    parent_records: Vec<u64>,
}

/// Stage timings of [`VolumeIndexBuilder::finish_timed`], in milliseconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct FinishTimings {
    /// Parent resolution + EXCLUDED propagation.
    pub build_ms: u64,
    /// The three permutation sorts.
    pub sort_ms: u64,
}

impl VolumeIndexBuilder {
    /// `volume_label` is the root display name, e.g. `C:`.
    /// `root_record` is the MFT record number of the root directory (5 on NTFS).
    pub fn new(volume_label: &str, root_record: u64) -> Self {
        let mut idx = VolumeIndex {
            name_pool: Vec::new(),
            lower_pool: Vec::new(),
            name_off: Vec::new(),
            name_len: Vec::new(),
            parent: Vec::new(),
            size: Vec::new(),
            mtime: Vec::new(),
            frn: Vec::new(),
            flag: Vec::new(),
            frn_map: FxHashMap::default(),
            perm_name: Vec::new(),
            perm_size: Vec::new(),
            perm_mtime: Vec::new(),
            content_generation: 0,
            structural_generation: 0,
            tombstones: 0,
            derived_cache: Mutex::new(None),
        };
        let units: Vec<u16> = volume_label.encode_utf16().collect();
        let root = idx.push_raw(&RawEntry {
            record: root_record,
            parent_record: u64::MAX, // resolves to nothing → NO_PARENT below
            frn: root_record,
            name_utf16: &units,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        debug_assert_eq!(root, VolumeIndex::ROOT);
        idx.parent[root as usize] = NO_PARENT;
        idx.frn_map.insert(masked(root_record), root);
        Self {
            idx,
            parent_records: vec![u64::MAX],
        }
    }

    pub fn push(&mut self, e: RawEntry) {
        let id = self.idx.push_raw(&e);
        self.idx.frn_map.insert(masked(e.record), id);
        self.parent_records.push(e.parent_record);
        debug_assert_eq!(self.parent_records.len(), id as usize + 1);
    }

    pub fn len(&self) -> usize {
        self.idx.len()
    }

    pub fn is_empty(&self) -> bool {
        self.idx.is_empty()
    }

    pub fn finish(self) -> VolumeIndex {
        self.finish_timed().0
    }

    pub fn finish_timed(mut self) -> (VolumeIndex, FinishTimings) {
        use rayon::prelude::*;

        let mut timings = FinishTimings::default();
        let t = std::time::Instant::now();

        // Pass 2: resolve parents now that every record is in the map.
        for i in 1..self.idx.len() {
            let p = self
                .idx
                .frn_map
                .get(&masked(self.parent_records[i]))
                .copied()
                .unwrap_or(VolumeIndex::ROOT);
            self.idx.parent[i] = p;
        }

        // Pass 3: propagate EXCLUDED down resolved parent chains. `done`
        // marks finalized entries; the stack unwinds each unresolved chain
        // top-down exactly once → O(n) total.
        {
            let n = self.idx.len();
            let mut done = vec![false; n];
            let root = VolumeIndex::ROOT as usize;
            self.idx.recompute_excluded(VolumeIndex::ROOT);
            done[root] = true;
            let mut stack: Vec<EntryId> = Vec::new();
            for start in 0..n as u32 {
                let mut cur = start;
                while !done[cur as usize] && stack.len() < 4096 {
                    stack.push(cur);
                    cur = self.idx.parent[cur as usize];
                    if cur == NO_PARENT {
                        break;
                    }
                }
                while let Some(id) = stack.pop() {
                    self.idx.recompute_excluded(id);
                    done[id as usize] = true;
                }
            }
        }

        timings.build_ms = t.elapsed().as_millis() as u64;
        let t = std::time::Instant::now();

        let ids: Vec<EntryId> = (0..self.idx.len() as u32).collect();
        let idx = &self.idx;
        let mut perm_name = ids.clone();
        let mut perm_size = ids.clone();
        let mut perm_mtime = ids;
        perm_name.par_sort_unstable_by(|&a, &b| idx.cmp_by(SortKey::Name, a, b));
        perm_size.par_sort_unstable_by(|&a, &b| idx.cmp_by(SortKey::Size, a, b));
        perm_mtime.par_sort_unstable_by(|&a, &b| idx.cmp_by(SortKey::Mtime, a, b));
        self.idx.perm_name = perm_name;
        self.idx.perm_size = perm_size;
        self.idx.perm_mtime = perm_mtime;
        timings.sort_ms = t.elapsed().as_millis() as u64;
        (self.idx, timings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testutil::{build_sample, raw, raw_attr, u16s};

    #[test]
    fn parents_resolve_across_scan_order() {
        let idx = build_sample();
        let note = idx.entry_by_record(100).unwrap();
        let docs = idx.entry_by_record(50).unwrap();
        assert_eq!(idx.parent(note), docs);
        assert_eq!(idx.parent(docs), VolumeIndex::ROOT);
    }

    #[test]
    fn orphan_attaches_to_root() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let name = u16s("lost.txt");
        b.push(raw(7, 999_999, &name, false, 1, 1));
        let idx = b.finish();
        let id = idx.entry_by_record(7).unwrap();
        assert_eq!(idx.parent(id), VolumeIndex::ROOT);
    }

    #[test]
    fn excluded_propagates_down_branches() {
        // C:\ ├─ $Recycle.Bin\(system dir) │ └─ sub\ │   └─ ghost.txt(plain)
        //     ├─ .git(hidden file)  └─ normal.txt
        let bin = u16s("$Recycle.Bin");
        let sub = u16s("sub");
        let ghost = u16s("ghost.txt");
        let git = u16s(".git");
        let normal = u16s("normal.txt");
        let mut b = VolumeIndexBuilder::new("C:", 5);
        // Push the deep child FIRST so propagation must survive scan order.
        b.push(raw_attr(30, 20, &ghost, false, false, false));
        b.push(raw_attr(20, 10, &sub, true, false, false));
        b.push(raw_attr(10, 5, &bin, true, false, true)); // system
        b.push(raw_attr(40, 5, &git, false, true, false)); // hidden
        b.push(raw_attr(50, 5, &normal, false, false, false));
        let idx = b.finish();

        for (rec, want) in [(10, true), (20, true), (30, true), (40, true), (50, false)] {
            let id = idx.entry_by_record(rec).unwrap();
            assert_eq!(idx.is_excluded(id), want, "record {rec}");
        }
    }
}
