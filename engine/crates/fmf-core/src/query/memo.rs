use rayon::prelude::*;

use crate::index::{EntryId, VolumeIndex};

// ── Pool offset table (generation-cached) ───────────────────────────────

/// Sorted (pool offset → entry id) table that maps pool-scan hits back to
/// entries. `name_off` loses monotonicity after renames (new names append),
/// so the sorted copy is rebuilt per content generation via the index's
/// derived-cache slot.
pub(super) struct OffsetTable {
    pub(super) offs: Vec<u32>,
    pub(super) ids: Vec<EntryId>,
}

impl OffsetTable {
    pub(super) fn build(idx: &VolumeIndex) -> Self {
        let mut pairs: Vec<(u32, EntryId)> = (0..idx.len() as u32)
            .map(|id| (idx.name_off_of(id), id))
            .collect();
        pairs.par_sort_unstable();
        OffsetTable {
            offs: pairs.iter().map(|p| p.0).collect(),
            ids: pairs.iter().map(|p| p.1).collect(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.offs.len()
    }
}

// ── Dir-path memo (generation-cached) ───────────────────────────────────

/// Memoized full paths for every directory (only built when the query
/// contains path terms). Entry paths are `memo[parent] + name`.
pub(super) struct DirPaths {
    pub(super) lower: Vec<Option<Box<[u8]>>>,
    pub(super) orig: Vec<Option<Box<[u8]>>>,
}

impl DirPaths {
    /// Level-order parallel build: a directory's path depends only on its
    /// parent's (one level up), so each depth level fans out across cores.
    pub(super) fn build(idx: &VolumeIndex) -> Self {
        let n = idx.len();
        let mut memo = DirPaths {
            lower: vec![None; n],
            orig: vec![None; n],
        };

        // Depth per directory via memoized chain walks (serial, O(n)).
        let mut depth: Vec<u32> = vec![u32::MAX; n];
        let mut stack: Vec<EntryId> = Vec::new();
        let mut max_depth = 0u32;
        for id in 0..n as u32 {
            if !idx.is_dir(id) {
                continue;
            }
            let mut cur = id;
            stack.clear();
            while depth[cur as usize] == u32::MAX {
                stack.push(cur);
                if cur == VolumeIndex::ROOT {
                    break;
                }
                cur = idx.parent(cur);
                if stack.len() > 4096 {
                    break; // corrupt parent cycle — treat as root-attached
                }
            }
            while let Some(d) = stack.pop() {
                let v = if d == VolumeIndex::ROOT {
                    0
                } else {
                    depth[idx.parent(d) as usize].saturating_add(1)
                };
                depth[d as usize] = v;
                max_depth = max_depth.max(v);
            }
        }

        let mut levels: Vec<Vec<EntryId>> = vec![Vec::new(); max_depth as usize + 1];
        for id in 0..n as u32 {
            if idx.is_dir(id) && depth[id as usize] != u32::MAX {
                levels[depth[id as usize] as usize].push(id);
            }
        }

        type BuiltDir = (EntryId, Box<[u8]>, Box<[u8]>);
        for level in levels {
            let built: Vec<BuiltDir> = level
                .into_par_iter()
                .map(|d| {
                    let (mut lower, mut orig) = if d == VolumeIndex::ROOT {
                        (Vec::new(), Vec::new())
                    } else {
                        let p = idx.parent(d) as usize;
                        (
                            memo.lower[p].as_deref().unwrap_or(&[]).to_vec(),
                            memo.orig[p].as_deref().unwrap_or(&[]).to_vec(),
                        )
                    };
                    lower.extend_from_slice(idx.lower_name(d));
                    lower.push(b'\\');
                    orig.extend_from_slice(idx.name(d));
                    orig.push(b'\\');
                    (d, lower.into_boxed_slice(), orig.into_boxed_slice())
                })
                .collect();
            for (d, lower, orig) in built {
                memo.lower[d as usize] = Some(lower);
                memo.orig[d as usize] = Some(orig);
            }
        }
        memo
    }

    #[inline]
    pub(super) fn parent_prefix(pool: &[Option<Box<[u8]>>], parent: EntryId) -> &[u8] {
        pool.get(parent as usize)
            .and_then(|p| p.as_deref())
            .unwrap_or(&[])
    }
}

/// Build the generation-cached query accelerators (dir-path memo + pool
/// offset table) ahead of the first query — the engine calls this once a
/// volume turns Ready so no keystroke ever pays the cold cost.
pub(crate) fn prewarm(idx: &VolumeIndex) {
    let _ = idx.cached_derived(|| DirPaths::build(idx));
    let _: std::sync::Arc<OffsetTable> = idx.cached_derived(|| OffsetTable::build(idx));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::index::VolumeIndexBuilder;
    use crate::index::testutil::{build_sample, raw, u16s};

    /// 60-deep dir chain (well inside both the memo's 4096 and
    /// append_parent_path's 128 depth bounds) plus a multibyte dir and files.
    fn deep_index() -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        for i in 0..60u64 {
            let name = u16s(&format!("d{i:02}"));
            let parent = if i == 0 { 5 } else { 99 + i };
            b.push(raw(100 + i, parent, &name, true, 0, i as i64));
        }
        let jp = u16s("日本語フォルダ");
        b.push(raw(300, 110, &jp, true, 0, 1)); // under d10
        let note = u16s("Note.TXT");
        b.push(raw(301, 300, &note, false, 9, 2));
        let leaf = u16s("leaf.txt");
        b.push(raw(302, 159, &leaf, false, 1, 3)); // under d59
        b.finish()
    }

    /// Oracle: full path of `id` incl. trailing `\`, built from the parent
    /// chain exactly like `VolumeIndex::append_path` does.
    fn oracle_paths(idx: &VolumeIndex, id: EntryId) -> (Vec<u8>, Vec<u8>) {
        let mut chain = vec![id];
        let mut cur = id;
        while cur != VolumeIndex::ROOT {
            cur = idx.parent(cur);
            chain.push(cur);
        }
        let (mut lower, mut orig) = (Vec::new(), Vec::new());
        for &c in chain.iter().rev() {
            lower.extend_from_slice(idx.lower_name(c));
            lower.push(b'\\');
            orig.extend_from_slice(idx.name(c));
            orig.push(b'\\');
        }
        (lower, orig)
    }

    fn assert_memo_matches_oracle(idx: &VolumeIndex) {
        let memo = DirPaths::build(idx);
        for id in 0..idx.len() as u32 {
            if idx.is_dir(id) {
                let (lower, orig) = oracle_paths(idx, id);
                assert_eq!(
                    memo.lower[id as usize].as_deref(),
                    Some(lower.as_slice()),
                    "lower path of dir {id}"
                );
                assert_eq!(
                    memo.orig[id as usize].as_deref(),
                    Some(orig.as_slice()),
                    "orig path of dir {id}"
                );
                // And the oracle itself agrees with the core path builder.
                // (append_path skips the root's own name — the volume label
                // is rendered by callers via name(); see engine/results.rs —
                // so the cross-check only applies below the root.)
                if id != VolumeIndex::ROOT {
                    let mut ap = Vec::new();
                    idx.append_path(id, &mut ap);
                    ap.push(b'\\');
                    assert_eq!(orig, ap, "append_path oracle of dir {id}");
                }
            } else {
                assert!(memo.lower[id as usize].is_none());
                assert!(memo.orig[id as usize].is_none());
            }
        }
    }

    #[test]
    fn dir_paths_match_append_path_oracle() {
        assert_memo_matches_oracle(&deep_index());
    }

    #[test]
    fn dir_paths_follow_dir_rename_and_reparent() {
        let mut idx = deep_index();
        // In-place rename of a mid-chain dir: every descendant path shifts.
        let renamed = u16s("Renamed_D10");
        idx.rename_dir_in_place(110, &renamed, 109).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_memo_matches_oracle(&idx);
        // Move a subtree (d30 under d02): depths change levels.
        idx.reparent(130, 102).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        assert_memo_matches_oracle(&idx);
    }

    #[test]
    fn offset_table_reflects_non_monotonic_name_off_after_dir_rename() {
        let mut idx = build_sample();
        let docs = idx.entry_by_record(50).unwrap();
        let zzz = u16s("zzz_docs");
        idx.rename_dir_in_place(50, &zzz, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);

        // Precondition: the rename made raw name_off non-monotonic.
        let raw_offs: Vec<u32> = (0..idx.len() as u32)
            .map(|id| idx.name_off_of(id))
            .collect();
        assert!(!raw_offs.is_sorted());

        let table = OffsetTable::build(&idx);
        assert_eq!(table.len(), idx.len());
        assert!(table.offs.is_sorted());
        let mut ids = table.ids.clone();
        ids.sort_unstable();
        assert_eq!(ids, (0..idx.len() as u32).collect::<Vec<_>>());
        for (off, &id) in table.offs.iter().zip(&table.ids) {
            assert_eq!(*off, idx.name_off_of(id), "table pair for entry {id}");
        }
        // The renamed dir's name was appended, so it sorts to the end.
        assert_eq!(*table.ids.last().unwrap(), docs);
    }

    #[test]
    fn cached_derived_invalidates_on_content_generation_change() {
        let mut idx = build_sample();
        let d1 = idx.cached_derived(|| DirPaths::build(&idx));
        let d2: Arc<DirPaths> = idx.cached_derived(|| unreachable!("cache hit expected"));
        assert!(Arc::ptr_eq(&d1, &d2));
        // A second cached type joins the generation without evicting the first.
        let t1 = idx.cached_derived(|| OffsetTable::build(&idx));
        let d3: Arc<DirPaths> = idx.cached_derived(|| unreachable!("cache hit expected"));
        assert!(Arc::ptr_eq(&d1, &d3));

        idx.merge_new_into_permutations(idx.len() as u32); // no-op batch: gen +1
        let d4 = idx.cached_derived(|| DirPaths::build(&idx));
        assert!(
            !Arc::ptr_eq(&d1, &d4),
            "DirPaths must rebuild on a new generation"
        );
        let t2 = idx.cached_derived(|| OffsetTable::build(&idx));
        assert!(
            !Arc::ptr_eq(&t1, &t2),
            "OffsetTable must rebuild on a new generation"
        );
    }

    #[test]
    fn prewarm_populates_both_derived_caches() {
        let idx = build_sample();
        prewarm(&idx);
        let _: Arc<DirPaths> = idx.cached_derived(|| unreachable!("DirPaths not prewarmed"));
        let _: Arc<OffsetTable> = idx.cached_derived(|| unreachable!("OffsetTable not prewarmed"));
    }
}
