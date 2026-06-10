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
