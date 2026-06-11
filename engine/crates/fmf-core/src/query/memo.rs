use rayon::prelude::*;

use crate::index::{EntryId, VolumeIndex};

// ── Pool offset table (generation-cached) ───────────────────────────────

/// Sorted (pool offset → entry id) table that maps pool-scan hits back to
/// entries. `name_off` loses monotonicity after renames (new names append),
/// so a sorted view is maintained per content generation via the index's
/// derived-cache slot — incrementally when possible ([`Self::extend_from`]):
/// pool appends are always at the tail, so a generation step is "copy +
/// append", never a re-sort.
///
/// Pairs whose entry has since moved its name (in-place dir renames) stay in
/// the table as *stale pairs*; the sweep detects them by comparing the
/// pair's offset against the entry's current `name_off` and skips the
/// region, exactly like the stale byte gaps it already handles.
pub(super) struct OffsetTable {
    pub(super) offs: Vec<u32>,
    pub(super) ids: Vec<EntryId>,
    /// Entries `[0, covers_entries)` are represented (possibly stale).
    covers_entries: u32,
    /// Pool bytes `[0, covers_pool_len)` are represented.
    covers_pool_len: u32,
    stale_pairs: u32,
}

/// Past this stale fraction the copy-and-append loses to a clean rebuild.
const STALE_REBUILD_DIVISOR: u32 = 8;

impl OffsetTable {
    pub(super) fn build(idx: &VolumeIndex) -> Self {
        let mut pairs: Vec<(u32, EntryId)> = (0..idx.len() as u32)
            .map(|id| (idx.name_off_of(id), id))
            .collect();
        pairs.par_sort_unstable();
        OffsetTable {
            offs: pairs.iter().map(|p| p.0).collect(),
            ids: pairs.iter().map(|p| p.1).collect(),
            covers_entries: idx.len() as u32,
            covers_pool_len: idx.name_pool_bytes().len() as u32,
            stale_pairs: 0,
        }
    }

    /// Extend `prev` to the current generation without sorting: keep its
    /// pairs (reusing the allocations when the cache held the last Arc,
    /// copying otherwise), then append the pairs past its watermarks.
    /// Appended entries arrive in offset order by construction; in-place
    /// renamed dirs (their `name_off` jumped past the watermark) are merged
    /// in and their old pairs left behind as stale.
    pub(super) fn extend_from(idx: &VolumeIndex, prev: std::sync::Arc<OffsetTable>) -> Self {
        let n = idx.len() as u32;
        let pool_len = idx.name_pool_bytes().len() as u32;
        // Entries and pool are append-only within a structural generation —
        // a regressed watermark means the cache got crossed with a different
        // index. Rebuilding recovers; the fact must not vanish.
        if prev.covers_entries > n || prev.covers_pool_len > pool_len {
            crate::metrics::Counters::bump_offset_table_rebuild_fallbacks();
            tracing::warn!(
                covers_entries = prev.covers_entries,
                entries = n,
                covers_pool = prev.covers_pool_len,
                pool = pool_len,
                "offset table watermark regressed — falling back to a full rebuild"
            );
            return Self::build(idx);
        }

        // Appended entries: ids past the watermark; their offsets ascend
        // with id (names append). In-place renamed dirs: old ids whose
        // current offset jumped past the pool watermark.
        let new_ids: Vec<EntryId> = (prev.covers_entries..n).collect();
        let mut moved: Vec<(u32, EntryId)> = (0..prev.covers_entries)
            .filter(|&id| idx.name_off_of(id) >= prev.covers_pool_len)
            .map(|id| (idx.name_off_of(id), id))
            .collect();
        moved.sort_unstable();

        let stale_pairs = prev.stale_pairs + moved.len() as u32;
        if stale_pairs > n / STALE_REBUILD_DIVISOR {
            tracing::debug!(
                stale_pairs,
                n,
                "offset table stale fraction high — clean rebuild"
            );
            return Self::build(idx);
        }

        // In the common case no in-flight query still holds the previous
        // table (the cache slot owned the only Arc) — reuse its allocations
        // and skip the multi-MB copy entirely.
        let added = new_ids.len() + moved.len();
        let (mut offs, mut ids) = match std::sync::Arc::try_unwrap(prev) {
            Ok(owned) => (owned.offs, owned.ids),
            Err(shared) => (shared.offs.clone(), shared.ids.clone()),
        };
        offs.reserve(added);
        ids.reserve(added);

        // Two-pointer merge of the two off-sorted append lists.
        let (mut i, mut j) = (0, 0);
        while i < new_ids.len() || j < moved.len() {
            let take_new = j >= moved.len()
                || (i < new_ids.len() && idx.name_off_of(new_ids[i]) <= moved[j].0);
            if take_new {
                offs.push(idx.name_off_of(new_ids[i]));
                ids.push(new_ids[i]);
                i += 1;
            } else {
                offs.push(moved[j].0);
                ids.push(moved[j].1);
                j += 1;
            }
        }
        debug_assert!(offs.is_sorted());

        OffsetTable {
            offs,
            ids,
            covers_entries: n,
            covers_pool_len: pool_len,
            stale_pairs,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.offs.len()
    }
}

// ── Dir-path memo (generation-cached) ───────────────────────────────────

/// Memoized full paths for every directory (only built when the query
/// contains path terms). Entry paths are `memo[parent] + name`.
///
/// Across generations the memo extends incrementally as long as no
/// existing directory was renamed or moved (`dir_topology_generation`):
/// appends never change old dir paths, so new dirs just memoize on top.
/// A topology change rebuilds from scratch — dir renames are rare and the
/// alternative (subtree invalidation) is not worth its complexity.
pub(super) struct DirPaths {
    pub(super) lower: Vec<Option<Box<[u8]>>>,
    pub(super) orig: Vec<Option<Box<[u8]>>>,
    /// Entries `[0, covers_entries)` are memoized.
    covers_entries: usize,
    /// The dir-topology generation this memo is valid for.
    topo_generation: u64,
}

impl DirPaths {
    /// Placeholder for queries without path terms — never read.
    pub(super) fn empty() -> Self {
        DirPaths {
            lower: Vec::new(),
            orig: Vec::new(),
            covers_entries: 0,
            topo_generation: 0,
        }
    }

    /// Extend `prev` to the current generation: memoize only the appended
    /// dirs. Their parents were live entries when pushed (lower id or
    /// root), so one increasing-id pass resolves every prefix. Falls back
    /// to a full build when a dir was renamed/moved (paths of arbitrary
    /// descendants changed) — the normal policy switch, not a degradation.
    pub(super) fn extend_from(idx: &VolumeIndex, prev: std::sync::Arc<DirPaths>) -> Self {
        let n = idx.len();
        if prev.topo_generation != idx.dir_topology_generation() || prev.covers_entries > n {
            return Self::build(idx);
        }
        let mut memo = match std::sync::Arc::try_unwrap(prev) {
            Ok(owned) => owned,
            Err(shared) => DirPaths {
                lower: shared.lower.clone(),
                orig: shared.orig.clone(),
                covers_entries: shared.covers_entries,
                topo_generation: shared.topo_generation,
            },
        };
        memo.lower.resize(n, None);
        memo.orig.resize(n, None);
        for id in memo.covers_entries as u32..n as u32 {
            if !idx.is_dir(id) {
                continue;
            }
            let p = idx.parent(id) as usize;
            let mut lower = memo
                .lower
                .get(p)
                .and_then(|x| x.as_deref())
                .unwrap_or(&[])
                .to_vec();
            let mut orig = memo
                .orig
                .get(p)
                .and_then(|x| x.as_deref())
                .unwrap_or(&[])
                .to_vec();
            lower.extend_from_slice(idx.lower_name(id));
            lower.push(b'\\');
            orig.extend_from_slice(idx.name(id));
            orig.push(b'\\');
            memo.lower[id as usize] = Some(lower.into_boxed_slice());
            memo.orig[id as usize] = Some(orig.into_boxed_slice());
        }
        memo.covers_entries = n;
        memo
    }

    /// Level-order parallel build: a directory's path depends only on its
    /// parent's (one level up), so each depth level fans out across cores.
    pub(super) fn build(idx: &VolumeIndex) -> Self {
        let n = idx.len();
        let mut memo = DirPaths {
            lower: vec![None; n],
            orig: vec![None; n],
            covers_entries: n,
            topo_generation: idx.dir_topology_generation(),
        };

        // Depth per directory via memoized chain walks (serial, O(n)).
        let mut depth: Vec<u32> = vec![u32::MAX; n];
        let mut stack: Vec<EntryId> = Vec::new();
        let mut max_depth = 0u32;
        let mut cycle_members = 0u64;
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
                    match depth[idx.parent(d) as usize] {
                        // Unresolved parent: `d` sits on a corrupt parent
                        // cycle (or a >4096 chain). Attach it at the root —
                        // propagating u32::MAX here used to poison max_depth
                        // and size the level table at ~4G entries (abort).
                        u32::MAX => {
                            cycle_members += 1;
                            1
                        }
                        pd => pd + 1,
                    }
                };
                depth[d as usize] = v;
                max_depth = max_depth.max(v);
            }
        }
        if cycle_members > 0 {
            // No MetricsHub at this layer; the WARN lands in the diag ring
            // (F12 panel + engine-error event), so the degradation is loud.
            tracing::warn!(
                cycle_members,
                "corrupt parent chain while building dir paths — affected dirs attached at root"
            );
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

/// Build the pool offset table ahead of the first query — the engine calls
/// this once a volume turns Ready so no keystroke pays the cold cost.
///
/// The dir-path memo is *not* prewarmed: most sessions never issue a path
/// query, and its footprint (full paths of every directory, ×2 pools) is
/// real RAM. The first keystroke containing `\` or `path:` builds it once;
/// after a USN batch it was already rebuilt lazily, so nothing regresses.
pub fn prewarm(idx: &VolumeIndex) {
    let _: std::sync::Arc<OffsetTable> = idx.cached_derived(|| OffsetTable::build(idx));
}

/// Bytes currently held by this index's derived caches (offset table +
/// dir-path memo), for the RAM accounting in `IndexStats`. Probes only —
/// never builds.
pub fn derived_cache_bytes(idx: &VolumeIndex) -> u64 {
    let mut total = 0u64;
    if let Some(t) = idx.derived_probe::<OffsetTable>() {
        total += (t.offs.capacity() * 4 + t.ids.capacity() * 4) as u64;
    }
    if let Some(d) = idx.derived_probe::<DirPaths>() {
        let slots =
            (d.lower.capacity() + d.orig.capacity()) * std::mem::size_of::<Option<Box<[u8]>>>();
        let boxed: usize = d
            .lower
            .iter()
            .chain(d.orig.iter())
            .filter_map(|p| p.as_ref().map(|b| b.len()))
            .sum();
        total += (slots + boxed) as u64;
    }
    total
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

    /// Oracle: an incrementally extended dir-path memo must equal a fresh
    /// build — across appended dirs (extend fast path) and dir renames /
    /// moves (topology bump → internal full rebuild).
    #[test]
    fn extended_dir_paths_match_fresh_build() {
        let assert_same_as_fresh = |idx: &VolumeIndex, memo: &DirPaths, what: &str| {
            let fresh = DirPaths::build(idx);
            for id in 0..idx.len() {
                assert_eq!(memo.lower[id], fresh.lower[id], "{what}: lower of {id}");
                assert_eq!(memo.orig[id], fresh.orig[id], "{what}: orig of {id}");
            }
        };

        let mut idx = deep_index();
        let memo = DirPaths::build(&idx);

        // Gen step 1: append a new dir under an existing one, a file in it,
        // and a nested dir under the *new* dir (parent inside the batch).
        let first_new = idx.len() as u32;
        let new_dir = u16s("new_dir");
        idx.upsert(&raw(500, 110, &new_dir, true, 0, 1));
        let new_file = u16s("new_file.txt");
        idx.upsert(&raw(501, 500, &new_file, false, 1, 2));
        let nested = u16s("nested");
        idx.upsert(&raw(502, 500, &nested, true, 0, 3));
        idx.merge_new_into_permutations(first_new);
        let memo = DirPaths::extend_from(&idx, Arc::new(memo));
        assert_same_as_fresh(&idx, &memo, "append generation");

        // Gen step 2: in-place dir rename — topology bump, extend must
        // rebuild and descendants must reflect the new name.
        let renamed = u16s("renamed_mid");
        idx.rename_dir_in_place(110, &renamed, 109).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let memo = DirPaths::extend_from(&idx, Arc::new(memo));
        assert_same_as_fresh(&idx, &memo, "rename generation");

        // Gen step 3: dir move (reparent) — also a topology bump.
        idx.reparent(500, 100).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let memo = DirPaths::extend_from(&idx, Arc::new(memo));
        assert_same_as_fresh(&idx, &memo, "reparent generation");

        // File-only batches keep the fast path: same topology generation.
        let first_new = idx.len() as u32;
        let f2 = u16s("plain.txt");
        idx.upsert(&raw(503, 100, &f2, false, 1, 4));
        idx.merge_new_into_permutations(first_new);
        let topo_before = idx.dir_topology_generation();
        let memo = DirPaths::extend_from(&idx, Arc::new(memo));
        assert_eq!(idx.dir_topology_generation(), topo_before);
        assert_same_as_fresh(&idx, &memo, "file-only generation");
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

    /// Oracle: an incrementally extended table must yield exactly the same
    /// sweep results as a freshly built one, across a chain of generations
    /// with appends, file renames (tombstone+append) and in-place dir
    /// renames. The tables themselves differ (stale pairs) — the *results*
    /// must not.
    #[test]
    fn extended_table_sweeps_like_a_fresh_build() {
        use super::super::compile::Driver;
        use super::super::sweep::driver_candidates;
        use memchr::memmem;

        let assert_equiv = |idx: &VolumeIndex, table: &OffsetTable, what: &str| {
            let fresh = OffsetTable::build(idx);
            let drivers = [
                Driver::Sub {
                    finder: memmem::Finder::new(b"doc").into_owned(),
                    needle_len: 3,
                    folded: true,
                },
                Driver::Sub {
                    finder: memmem::Finder::new(b"renamed").into_owned(),
                    needle_len: 7,
                    folded: true,
                },
                Driver::Prefix {
                    bytes: b"note".to_vec(),
                    folded: true,
                },
                Driver::Suffixes {
                    suffixes: vec![b".txt".to_vec()],
                    folded: true,
                    files_only: true,
                },
            ];
            for (i, d) in drivers.iter().enumerate() {
                let mut a = driver_candidates(idx, table, d, true);
                let mut b = driver_candidates(idx, &fresh, d, true);
                a.sort_unstable();
                b.sort_unstable();
                assert_eq!(a, b, "{what}: driver {i} diverged from fresh build");
            }
        };

        // ~100 entries so the stale threshold (n/8) tolerates a few in-place
        // renames instead of flipping straight to a rebuild.
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let dir = u16s("docs_dir");
        b.push(raw(50, 5, &dir, true, 0, 1));
        for i in 0..100u64 {
            let name = if i % 3 == 0 {
                u16s(&format!("note_{i:03}_doc.txt"))
            } else {
                u16s(&format!("file_{i:03}.log"))
            };
            let parent = if i % 2 == 0 { 50 } else { 5 };
            b.push(raw(100 + i, parent, &name, false, i, i as i64));
        }
        let mut idx = b.finish();
        let mut table = OffsetTable::build(&idx);

        // Gen step 1: append a new file + rename an existing file
        // (tombstone + append, same record).
        let first_new = idx.len() as u32;
        let extra = u16s("docs_extra.txt");
        idx.upsert(&raw(9000, 5, &extra, false, 1, 7));
        let renamed_file = u16s("note_renamed.txt");
        idx.upsert(&raw(103, 50, &renamed_file, false, 9, 8));
        idx.merge_new_into_permutations(first_new);
        table = OffsetTable::extend_from(&idx, Arc::new(table));
        assert_eq!(table.stale_pairs, 0, "appends produce no stale pairs");
        assert_equiv(&idx, &table, "append generation");

        // Gen step 2: in-place dir rename — the moved name strands a stale
        // pair at its old offset.
        let zzz = u16s("zzz_docs_renamed");
        idx.rename_dir_in_place(50, &zzz, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        table = OffsetTable::extend_from(&idx, Arc::new(table));
        assert_eq!(table.stale_pairs, 1);
        assert!(table.offs.is_sorted());
        assert_equiv(&idx, &table, "dir-rename generation");

        // Gen step 3: both at once.
        let first_new = idx.len() as u32;
        let more = u16s("more_docs.log");
        idx.upsert(&raw(9001, 50, &more, false, 2, 9));
        idx.merge_new_into_permutations(first_new);
        let back = u16s("docs_back");
        idx.rename_dir_in_place(50, &back, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        table = OffsetTable::extend_from(&idx, Arc::new(table));
        assert_eq!(table.stale_pairs, 2);
        assert_equiv(&idx, &table, "mixed generation");
    }

    /// A high stale fraction flips the policy to a clean rebuild — same
    /// results, watermark reset.
    #[test]
    fn extend_falls_back_to_rebuild_past_stale_threshold() {
        let mut idx = build_sample(); // 4 entries → threshold n/8 = 0
        let table = OffsetTable::build(&idx);
        let renamed = u16s("docs_renamed");
        idx.rename_dir_in_place(50, &renamed, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);

        let extended = OffsetTable::extend_from(&idx, Arc::new(table));
        assert_eq!(
            extended.stale_pairs, 0,
            "1 stale > 4/8 entries — must have rebuilt clean"
        );
        assert_eq!(extended.len(), idx.len());
        for (off, &id) in extended.offs.iter().zip(&extended.ids) {
            assert_eq!(*off, idx.name_off_of(id));
        }
    }

    #[test]
    fn prewarm_builds_offset_table_but_leaves_dir_paths_lazy() {
        let idx = build_sample();
        prewarm(&idx);
        let _: Arc<OffsetTable> = idx.cached_derived(|| unreachable!("OffsetTable not prewarmed"));
        // The dir-path memo only materializes on the first path query.
        assert!(idx.derived_probe::<DirPaths>().is_none());
        assert!(derived_cache_bytes(&idx) > 0, "offset table accounted");
        let before = derived_cache_bytes(&idx);
        let _ = idx.cached_derived(|| DirPaths::build(&idx));
        assert!(
            derived_cache_bytes(&idx) > before,
            "dir-path memo joins the accounting once built"
        );
    }

    #[test]
    fn parent_cycle_attaches_dirs_at_root_instead_of_aborting() {
        // Corrupt USN records can produce a parent cycle (a→b→a). The
        // unresolved depth used to propagate as u32::MAX into max_depth,
        // sizing the level table at ~4G entries — an allocation abort. Cycle
        // members must instead come out root-attached, with paths intact.
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (da, db, f) = (u16s("a"), u16s("b"), u16s("f.txt"));
        b.push(raw(10, 5, &da, true, 0, 1));
        b.push(raw(20, 10, &db, true, 0, 2));
        b.push(raw(30, 20, &f, false, 1, 3));
        let mut idx = b.finish();
        idx.reparent(10, 20); // a under b while b is under a — cycle
        let a = idx.entry_by_record(10).unwrap();
        let bb = idx.entry_by_record(20).unwrap();

        let memo = DirPaths::build(&idx);
        for d in [a, bb] {
            let lower = memo.lower[d as usize]
                .as_deref()
                .expect("cycle members still get a path");
            assert!(lower.ends_with(b"\\"));
        }
    }
}
