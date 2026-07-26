use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::frn::FrnIndex;
use super::{EncodedEntry, Frn, NO_PARENT, RawEntry, VolumeIndex, flags};

/// Two-pass builder for the initial scan: collect everything, then resolve
/// parents and sort the permutations (scan order ≠ parent-before-child).
pub struct VolumeIndexBuilder {
    idx: VolumeIndex,
    parent_frns: Vec<Frn>,
}

/// Stage timings of [`VolumeIndexBuilder::finish_timed`], in milliseconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct FinishTimings {
    /// Parent resolution + EXCLUDED propagation.
    pub build_ms: u64,
    /// The name-permutation sort.
    pub sort_ms: u64,
}

impl VolumeIndexBuilder {
    /// Drop duplicate `(full FRN, parent EntryId, original name)` rows emitted by
    /// corrupt/redundant attribute-list metadata. The temporary set is scoped
    /// to one record-number group, so the normal one-row case allocates
    /// nothing and even a million-entry volume avoids a second global map.
    fn tombstone_duplicate_links(&mut self) -> bool {
        let mut duplicates = Vec::new();
        let ids = self.idx.frn_index.sorted_ids();
        let mut start = 0usize;
        while start < ids.len() {
            let record = self.idx.frn(ids[start]).record();
            let mut end = start + 1;
            while end < ids.len() && self.idx.frn(ids[end]).record() == record {
                end += 1;
            }
            if end - start > 1 {
                let mut seen = rustc_hash::FxHashSet::default();
                for &id in &ids[start..end] {
                    let identity = (self.idx.frn(id), self.idx.parent(id), self.idx.name(id));
                    if !seen.insert(identity) {
                        duplicates.push(id);
                    }
                }
            }
            start = end;
        }
        let found = !duplicates.is_empty();
        for id in duplicates {
            self.idx.tombstone_id(id);
        }
        found
    }

    /// `volume_label` is the root display name, e.g. `C:`.
    /// `root_record` is the MFT record number of the root directory (5 on NTFS).
    #[must_use]
    pub fn new(volume_label: &str, root_record: u64) -> Self {
        let mut idx = VolumeIndex {
            dict_pool: Vec::new(),
            dict_off: Vec::new(),
            name_id: Vec::new(),
            orig_pool: Vec::new(),
            orig_off: Vec::new(),
            parent: Vec::new(),
            size_lo: Vec::new(),
            size_ovf: rustc_hash::FxHashMap::default(),
            mtime: Vec::new(),
            frn: Vec::new(),
            flag: Vec::new(),
            frn_index: FrnIndex::default(),
            perm_name: Vec::new(),
            content_generation: 0,
            structural_generation: 0,
            dir_topology_generation: 0,
            exclusion_tree_dirty: false,
            tombstones: 0,
            dead_name_bytes: 0,
            dict_appends_since_dedup: 0,
            derived_cache: Mutex::new(None),
        };
        let units: Vec<u16> = volume_label.encode_utf16().collect();
        // Parents are provisional ROOT during the build — finish() resolves
        // them all in one parallel pass once the FRN index exists.
        let root = idx.push_raw(
            &RawEntry {
                parent_frn: Frn(u64::MAX), // resolves to nothing → NO_PARENT below
                frn: Frn(root_record),
                name_utf16: &units,
                is_dir: true,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: 0,
                mtime: 0,
            },
            VolumeIndex::ROOT,
        );
        debug_assert_eq!(root, VolumeIndex::ROOT);
        idx.parent[root as usize] = NO_PARENT;
        Self {
            idx,
            parent_frns: vec![Frn(u64::MAX)],
        }
    }

    /// Append a scanned entry with a provisional ROOT parent; the real
    /// parent is resolved later in [`Self::finish`] once the FRN index exists.
    pub fn push(&mut self, e: RawEntry) {
        let id = self.idx.push_raw(&e, VolumeIndex::ROOT);
        self.parent_frns.push(e.parent_frn);
        debug_assert_eq!(self.parent_frns.len(), id as usize + 1);
    }

    /// Append an entry whose name was WTF-8 encoded off-thread (parallel
    /// scan workers). Identical semantics to [`Self::push`].
    pub fn push_encoded(&mut self, e: EncodedEntry) {
        let id = self.idx.push_encoded(&e, VolumeIndex::ROOT);
        self.parent_frns.push(e.parent_frn);
        debug_assert_eq!(self.parent_frns.len(), id as usize + 1);
    }

    /// The number of entries pushed so far (including the root).
    pub const fn len(&self) -> usize {
        self.idx.len()
    }

    /// True when no entries have been pushed (never true after [`Self::new`],
    /// which seeds the root).
    pub const fn is_empty(&self) -> bool {
        self.idx.is_empty()
    }

    /// Run the two-pass finalization and return the queryable [`VolumeIndex`],
    /// discarding the stage timings.
    pub fn finish(self) -> VolumeIndex {
        self.finish_timed().0
    }

    /// Run the two-pass finalization (resolve parents, propagate EXCLUDED,
    /// build the name sort) and return the [`VolumeIndex`] with stage timings.
    ///
    /// # Panics
    ///
    /// Panics only if the internal permanently-clear cancellation flag is
    /// observed as cancelled, which would indicate an implementation bug.
    pub fn finish_timed(self) -> (VolumeIndex, FinishTimings) {
        let stop = AtomicBool::new(false);
        self.finish_timed_cancellable(&stop)
            .expect("a permanently clear cancellation flag cannot cancel")
    }

    /// Cancellation-aware finalization used by the volume worker. Expensive
    /// parallel primitives are individually bounded; checks between stages
    /// ensure a stopped worker never publishes the partially finalized index.
    pub(crate) fn finish_timed_cancellable(
        mut self,
        stop: &AtomicBool,
    ) -> Option<(VolumeIndex, FinishTimings)> {
        use rayon::prelude::*;

        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let mut timings = FinishTimings::default();
        let t = std::time::Instant::now();

        // Pass 1.5: the FRN index, in one parallel sort (ADR-0005).
        self.idx.frn_index = FrnIndex::build(&self.idx.frn, &self.idx.flag);
        if stop.load(Ordering::Relaxed) {
            return None;
        }

        // Pass 2: resolve parents now that every record is findable.
        // Read-only lookups, one write per slot — embarrassingly parallel.
        {
            let VolumeIndex {
                frn_index,
                parent,
                frn,
                flag,
                ..
            } = &mut self.idx;
            let parent_frns = &self.parent_frns;
            let root_record = Frn(frn[VolumeIndex::ROOT as usize]).record();
            parent
                .par_iter_mut()
                .enumerate()
                .skip(1) // the root keeps NO_PARENT
                .for_each(|(i, p)| {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let wanted = parent_frns[i];
                    *p = frn_index
                        .lookup_all(wanted.record(), frn, flag)
                        .find(|&id| {
                            Frn(frn[id as usize]) == wanted
                                && flag[id as usize] & flags::IS_DIR != 0
                        })
                        // Deterministic fixtures and synthetic sources may
                        // carry a bare record number (sequence 0). Production
                        // MFT references include a sequence and stay exact.
                        .or_else(|| {
                            (wanted.0 >> 48 == 0).then(|| {
                                frn_index
                                    .lookup_all(wanted.record(), frn, flag)
                                    .find(|&id| flag[id as usize] & flags::IS_DIR != 0)
                            })?
                        })
                        // The builder seeds root from its bare record number;
                        // child $FILE_NAME references carry root's sequence.
                        .or_else(|| {
                            (wanted.record() == root_record
                                && flag[VolumeIndex::ROOT as usize] & flags::IS_DIR != 0)
                                .then_some(VolumeIndex::ROOT)
                        })
                        .unwrap_or(VolumeIndex::ROOT);
                });
        }
        // Link identity uses the resolved parent EntryId, not merely the raw
        // on-disk parent reference. This also coalesces equivalent seeded-root
        // references with/without a sequence number.
        let had_duplicate_links = self.tombstone_duplicate_links();
        if stop.load(Ordering::Relaxed) {
            return None;
        }

        // Pass 3: propagate EXCLUDED down resolved parent chains exactly once.
        // The same cycle-safe O(n) implementation runs at dirty USN batch
        // boundaries, so scan and incremental semantics cannot drift.
        self.idx.recompute_all_excluded();
        if stop.load(Ordering::Relaxed) {
            return None;
        }

        timings.build_ms = t.elapsed().as_millis() as u64;
        let t = std::time::Instant::now();

        // Collapse the per-entry name appends into the distinct-name
        // dictionary before sorting (ADR-0032); the name comparator below
        // reads each folded name through the deduped `name_id`. The
        // original-spelling pool is deduped in the same breath (ADR-0033
        // Lever 1) — independent of the sort, which only reads folded names.
        self.idx.dedup_dict();
        self.idx.dedup_orig();
        if stop.load(Ordering::Relaxed) {
            return None;
        }

        // Name order only — the default sort, needed before the volume can
        // serve its first query. Size/mtime orders build lazily on the
        // first sorted query (query::memo, ADR-0006).
        //
        // Rank the D distinct dictionary names once by bytes, then sort the
        // entries on a packed `(rank << 32) | id` u64 key (ADR-0033, "1A").
        // `dedup_dict` just ran, so `name_id` is dense `0..D` and `dict_off`
        // is gapless: distinct names map to distinct ranks, making this
        // byte-identical to a full `cmp_by(SortKey::Name)` sort (equal names
        // tie-break by id — the key's low 32 bits) while the comparator is a
        // single integer compare instead of a dictionary deref per compare.
        let n = self.idx.len();
        let d = self.idx.dict_off.len();
        let dict_pool = &self.idx.dict_pool;
        let dict_off = &self.idx.dict_off;
        let name_bytes = |nid: usize| {
            let off = dict_off[nid] as usize;
            let end = dict_off
                .get(nid + 1)
                .map_or(dict_pool.len(), |&e| e as usize);
            &dict_pool[off..end]
        };
        let mut order: Vec<u32> = (0..d as u32).collect();
        order.par_sort_unstable_by(|&a, &b| name_bytes(a as usize).cmp(name_bytes(b as usize)));
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let mut rank = vec![0u32; d];
        for (r, &nid) in order.iter().enumerate() {
            if r.trailing_zeros() >= 12 && stop.load(Ordering::Relaxed) {
                return None;
            }
            rank[nid as usize] = r as u32;
        }
        let name_id = &self.idx.name_id;
        let mut keyed: Vec<u64> = (0..n as u32)
            .map(|id| (u64::from(rank[name_id[id as usize] as usize]) << 32) | u64::from(id))
            .collect();
        keyed.par_sort_unstable();
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        self.idx.perm_name = keyed.iter().map(|&k| k as u32).collect();
        timings.sort_ms = t.elapsed().as_millis() as u64;
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        // Duplicate rows are exceptional. Keep the fast common path
        // allocation-free; only a corrupt/redundant source pays one compact
        // copy so the published initial index contains no defensive
        // tombstones or duplicate query rows.
        let idx = if had_duplicate_links {
            self.idx.compacted()
        } else {
            self.idx
        };
        Some((idx, timings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testutil::{build_sample, raw, raw_attr, u16s};
    use crate::index::{EntryId, SortKey};

    /// 1A build-rank (ADR-0033) must produce a permutation byte-identical to a
    /// full `cmp_by(SortKey::Name)` sort: the packed `(rank, id)` key has to
    /// reproduce the (folded-name, id) order exactly. Repeated folded names
    /// (so `D < n`), case variants that fold together, and a multibyte name
    /// exercise the dictionary-rank path and the id tie-break inside an
    /// equal-name run.
    #[test]
    fn name_permutation_matches_cmp_by_oracle() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let names = [
            "report.log",
            "report.log",
            "report.log",
            ".gitignore",
            ".gitignore",
            "README",
            "readme",
            "ReadMe",
            "日本語.txt",
            "日本語.txt",
            "alpha",
            "zulu",
            "mike",
            "a.bin",
        ];
        for (i, name) in names.iter().enumerate() {
            let units = u16s(name);
            b.push(raw(100 + i as u64, 5, &units, false, i as u64, i as i64));
        }
        let idx = b.finish();

        // Independent oracle: the one definition of name order, applied by a
        // plain comparator sort over every id.
        let mut oracle: Vec<EntryId> = (0..idx.len() as u32).collect();
        oracle.sort_by(|&a, &b| idx.cmp_by(SortKey::Name, a, b));
        assert_eq!(
            idx.name_permutation(),
            oracle.as_slice(),
            "build-rank permutation diverged from the cmp_by(Name) oracle"
        );
    }

    #[test]
    fn cancelled_finalization_never_returns_a_partial_index() {
        let b = VolumeIndexBuilder::new("C:", 5);
        let stop = AtomicBool::new(true);
        assert!(b.finish_timed_cancellable(&stop).is_none());
    }

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
    fn file_record_can_never_be_selected_as_a_parent() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let file = u16s("not-a-directory");
        let exact_child = u16s("exact.txt");
        let bare_child = u16s("bare.txt");
        b.push(raw(10, 5, &file, false, 1, 1));
        b.push(raw(20, 10, &exact_child, false, 1, 1));
        b.push(RawEntry {
            parent_frn: Frn(10),
            frn: Frn((1u64 << 48) | 0x15),
            name_utf16: &bare_child,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: 1,
        });

        let idx = b.finish();
        for record in [20, 21] {
            let child = idx.entry_by_record(record).unwrap();
            assert_eq!(
                idx.parent(child),
                VolumeIndex::ROOT,
                "both exact and sequence-zero parent lookup must reject files"
            );
        }
    }

    #[test]
    fn exact_duplicate_link_identity_is_published_once() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let name = u16s("once.txt");
        b.push(raw(10, 5, &name, false, 1, 1));
        b.push(RawEntry {
            parent_frn: Frn((1u64 << 48) | 5),
            frn: Frn((1u64 << 48) | 0x0A),
            name_utf16: &name,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: 1,
        });

        let idx = b.finish();

        assert_eq!(idx.len(), 2, "root plus one unique link row");
        assert_eq!(idx.live_len(), 2);
        assert_eq!(idx.entries_by_frn(Frn((1u64 << 48) | 0x0A)).count(), 1);
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
