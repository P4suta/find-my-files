use rayon::prelude::*;

use crate::index::{EntryId, SortKey, VolumeIndex};

// ── Lazy sort permutations (generation-cached) ──────────────────────────

/// Pre-sorted id order for one sort key, built on the first query that
/// sorts by it and extended per content generation after that — the same
/// insertion-point in-place merge the name permutation uses, through the
/// same `cmp_by` order (ADR-0006).
///
/// Never persisted: a snapshot restore re-sorts on first use, which also
/// resets any staleness in-place stat updates accumulated.
#[derive(Clone)]
pub(super) struct SortPerm {
    pub(super) ids: Vec<EntryId>,
    /// Entries `[0, covers)` are placed; a generation step sorts and
    /// merges only the ids past the watermark.
    covers: u32,
}

/// Size order — its own derived-cache slot (TypeId-keyed).
pub(super) struct SizePerm(pub(super) SortPerm);
/// Mtime order — separate slot.
pub(super) struct MtimePerm(pub(super) SortPerm);

impl SizePerm {
    pub(super) fn get(idx: &VolumeIndex) -> std::sync::Arc<Self> {
        idx.cached_derived_or_update(|prev| match prev {
            Some(p) => Self(SortPerm::extend(
                idx,
                take_perm(p, |m: &Self| &m.0),
                SortKey::Size,
            )),
            None => Self(SortPerm::build(idx, SortKey::Size)),
        })
    }
}

impl MtimePerm {
    pub(super) fn get(idx: &VolumeIndex) -> std::sync::Arc<Self> {
        idx.cached_derived_or_update(|prev| match prev {
            Some(p) => Self(SortPerm::extend(
                idx,
                take_perm(p, |m: &Self| &m.0),
                SortKey::Mtime,
            )),
            None => Self(SortPerm::build(idx, SortKey::Mtime)),
        })
    }
}

/// Reuse the previous permutation's allocation when the cache slot held
/// the only Arc, clone otherwise (same policy as the other derived caches).
fn take_perm<T>(prev: std::sync::Arc<T>, perm_of: impl Fn(&T) -> &SortPerm) -> SortPerm
where
    SortPerm: From<T>,
{
    match std::sync::Arc::try_unwrap(prev) {
        Ok(owned) => owned.into(),
        Err(shared) => perm_of(&shared).clone(),
    }
}

impl From<SizePerm> for SortPerm {
    fn from(p: SizePerm) -> Self {
        p.0
    }
}
impl From<MtimePerm> for SortPerm {
    fn from(p: MtimePerm) -> Self {
        p.0
    }
}

impl SortPerm {
    fn build(idx: &VolumeIndex, key: SortKey) -> Self {
        let mut ids: Vec<EntryId> = (0..idx.len() as u32).collect();
        ids.par_sort_unstable_by(|&a, &b| idx.cmp_by(key, a, b));
        Self {
            ids,
            covers: idx.len() as u32,
        }
    }

    fn extend(idx: &VolumeIndex, mut perm: Self, key: SortKey) -> Self {
        let n = idx.len() as u32;
        // Entries are append-only within a structural generation — a
        // regressed watermark means the cache got crossed with a different
        // index. Rebuilding recovers; the fact must not vanish.
        if perm.covers > n {
            crate::metrics::Counters::bump_lazy_perm_rebuild_fallbacks();
            tracing::warn!(
                covers = perm.covers,
                entries = n,
                "lazy sort permutation watermark regressed — falling back to a full rebuild"
            );
            return Self::build(idx, key);
        }
        let mut batch: Vec<EntryId> = (perm.covers..n).collect();
        batch.sort_unstable_by(|&a, &b| idx.cmp_by(key, a, b));
        crate::index::merge_sorted_tail(&mut perm.ids, &batch, |a, b| idx.cmp_by(key, a, b));
        perm.covers = n;
        perm
    }
}

// ── Dir-path memo (generation-cached, one per name pool) ────────────────

/// Memoized full paths for every directory, for one name pool. Only built
/// when the query contains path terms — and split per pool so a query only
/// pays for the pool(s) it reads (nearly all path queries are folded).
/// Entry paths are `memo[parent] + name`.
///
/// Across generations a memo extends incrementally as long as no existing
/// directory was renamed or moved (`dir_topology_generation`): appends
/// never change old dir paths, so new dirs just memoize on top. A topology
/// change rebuilds from scratch — dir renames are rare and the alternative
/// (subtree invalidation) is not worth its complexity.
pub(super) struct DirPathsPool {
    paths: Vec<Option<Box<[u8]>>>,
    /// Entries `[0, covers_entries)` are memoized.
    covers_entries: usize,
    /// The dir-topology generation this memo is valid for.
    topo_generation: u64,
}

/// Folded-name memo — its own derived-cache slot (TypeId-keyed).
pub(super) struct DirPathsLower(DirPathsPool);
/// Original-name memo — separate slot, built only by orig-case path terms.
pub(super) struct DirPathsOrig(DirPathsPool);

impl DirPathsLower {
    pub(super) fn build(idx: &VolumeIndex) -> Self {
        Self(DirPathsPool::build(idx, true))
    }

    pub(super) fn extend_from(idx: &VolumeIndex, prev: std::sync::Arc<Self>) -> Self {
        Self(DirPathsPool::extend(idx, take_pool(prev, |m| &m.0), true))
    }
}

impl DirPathsOrig {
    pub(super) fn build(idx: &VolumeIndex) -> Self {
        Self(DirPathsPool::build(idx, false))
    }

    pub(super) fn extend_from(idx: &VolumeIndex, prev: std::sync::Arc<Self>) -> Self {
        Self(DirPathsPool::extend(idx, take_pool(prev, |m| &m.0), false))
    }
}

/// Reuse the previous memo's allocations when the cache slot held the only
/// Arc (the common case — no in-flight query still reads it), clone
/// otherwise.
fn take_pool<T>(prev: std::sync::Arc<T>, pool_of: impl Fn(&T) -> &DirPathsPool) -> DirPathsPool
where
    DirPathsPool: From<T>,
{
    match std::sync::Arc::try_unwrap(prev) {
        Ok(owned) => owned.into(),
        Err(shared) => {
            let p = pool_of(&shared);
            DirPathsPool {
                paths: p.paths.clone(),
                covers_entries: p.covers_entries,
                topo_generation: p.topo_generation,
            }
        }
    }
}

impl From<DirPathsLower> for DirPathsPool {
    fn from(m: DirPathsLower) -> Self {
        m.0
    }
}
impl From<DirPathsOrig> for DirPathsPool {
    fn from(m: DirPathsOrig) -> Self {
        m.0
    }
}

impl DirPathsPool {
    #[inline]
    fn name_of(idx: &VolumeIndex, id: EntryId, folded: bool) -> &[u8] {
        if folded {
            idx.lower_name(id)
        } else {
            idx.name(id)
        }
    }

    /// Extend `pool` to the current generation: memoize only the appended
    /// dirs. Their parents were live entries when pushed (lower id or
    /// root), so one increasing-id pass resolves every prefix. Falls back
    /// to a full build when a dir was renamed/moved (paths of arbitrary
    /// descendants changed) — the normal policy switch, not a degradation.
    fn extend(idx: &VolumeIndex, mut pool: Self, folded: bool) -> Self {
        let n = idx.len();
        if pool.topo_generation != idx.dir_topology_generation() || pool.covers_entries > n {
            return Self::build(idx, folded);
        }
        pool.paths.resize(n, None);
        for id in pool.covers_entries as u32..n as u32 {
            if !idx.is_dir(id) {
                continue;
            }
            let p = idx.parent(id) as usize;
            let mut path = pool
                .paths
                .get(p)
                .and_then(|x| x.as_deref())
                .unwrap_or(&[])
                .to_vec();
            path.extend_from_slice(Self::name_of(idx, id, folded));
            path.push(b'\\');
            pool.paths[id as usize] = Some(path.into_boxed_slice());
        }
        pool.covers_entries = n;
        pool
    }

    /// Level-order parallel build: a directory's path depends only on its
    /// parent's (one level up), so each depth level fans out across cores.
    fn build(idx: &VolumeIndex, folded: bool) -> Self {
        let n = idx.len();
        let mut memo = Self {
            paths: vec![None; n],
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
                        // u32::MAX must not propagate into max_depth, which
                        // sizes the level table.
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

        for level in levels {
            let built: Vec<(EntryId, Box<[u8]>)> = level
                .into_par_iter()
                .map(|d| {
                    let mut path = if d == VolumeIndex::ROOT {
                        Vec::new()
                    } else {
                        let p = idx.parent(d) as usize;
                        memo.paths[p].as_deref().unwrap_or(&[]).to_vec()
                    };
                    path.extend_from_slice(Self::name_of(idx, d, folded));
                    path.push(b'\\');
                    (d, path.into_boxed_slice())
                })
                .collect();
            for (d, path) in built {
                memo.paths[d as usize] = Some(path);
            }
        }
        memo
    }

    #[inline]
    fn parent_prefix(&self, parent: EntryId) -> &[u8] {
        self.paths
            .get(parent as usize)
            .and_then(|p| p.as_deref())
            .unwrap_or(&[])
    }

    fn bytes(&self) -> u64 {
        let slots = self.paths.capacity() * std::mem::size_of::<Option<Box<[u8]>>>();
        let boxed: usize = self
            .paths
            .iter()
            .filter_map(|p| p.as_ref().map(|b| b.len()))
            .sum();
        (slots + boxed) as u64
    }
}

/// The path memos one query execution may read. `None` means the compiled
/// query proved it never reads that pool — most path queries are folded
/// and skip the original-name memo entirely.
#[derive(Default)]
pub(super) struct PathMemos {
    pub(super) lower: Option<std::sync::Arc<DirPathsLower>>,
    pub(super) orig: Option<std::sync::Arc<DirPathsOrig>>,
}

impl PathMemos {
    #[inline]
    pub(super) fn lower_prefix(&self, parent: EntryId) -> &[u8] {
        self.lower
            .as_ref()
            .map_or(&[][..], |m| m.0.parent_prefix(parent))
    }

    #[inline]
    pub(super) fn orig_prefix(&self, parent: EntryId) -> &[u8] {
        self.orig
            .as_ref()
            .map_or(&[][..], |m| m.0.parent_prefix(parent))
    }
}

/// Build the pool offset table ahead of the first query — the engine calls
/// this once a volume turns Ready so no keystroke pays the cold cost.
///
/// Prewarm derived caches at Ready. A no-op since ADR-0032 removed the
/// offset-table cache (the name dictionary is resident from build/restore):
/// the lazy sort and dir-path memos are intentionally built on demand — most
/// sessions never sort by size/mtime or issue a path query, and those memos'
/// footprint (full paths of every directory, ×2 pools) is real RAM.
pub const fn prewarm(_idx: &VolumeIndex) {}

/// Bytes currently held by this index's derived caches (dir-path memos and
/// the lazy sort permutations), for the RAM accounting in `IndexStats`.
/// Probes only — never builds.
pub fn derived_cache_bytes(idx: &VolumeIndex) -> u64 {
    let mut total = 0u64;
    if let Some(d) = idx.derived_probe::<DirPathsLower>() {
        total += d.0.bytes();
    }
    if let Some(d) = idx.derived_probe::<DirPathsOrig>() {
        total += d.0.bytes();
    }
    if let Some(p) = idx.derived_probe::<SizePerm>() {
        total += (p.0.ids.capacity() * 4) as u64;
    }
    if let Some(p) = idx.derived_probe::<MtimePerm>() {
        total += (p.0.ids.capacity() * 4) as u64;
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
    /// `append_parent_path`'s 128 depth bounds) plus a multibyte dir and files.
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
        let lower_memo = DirPathsLower::build(idx);
        let orig_memo = DirPathsOrig::build(idx);
        for id in 0..idx.len() as u32 {
            if idx.is_dir(id) {
                let (lower, orig) = oracle_paths(idx, id);
                assert_eq!(
                    lower_memo.0.paths[id as usize].as_deref(),
                    Some(lower.as_slice()),
                    "lower path of dir {id}"
                );
                assert_eq!(
                    orig_memo.0.paths[id as usize].as_deref(),
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
                assert!(lower_memo.0.paths[id as usize].is_none());
                assert!(orig_memo.0.paths[id as usize].is_none());
            }
        }
    }

    #[test]
    fn dir_paths_match_append_path_oracle() {
        assert_memo_matches_oracle(&deep_index());
    }

    /// Oracle: an incrementally extended dir-path memo must equal a fresh
    /// build — across appended dirs (extend fast path) and dir renames /
    /// moves (topology bump → internal full rebuild). Both pools, since
    /// they extend independently in their own cache slots.
    #[test]
    fn extended_dir_paths_match_fresh_build() {
        let assert_same_as_fresh =
            |idx: &VolumeIndex, lower: &DirPathsLower, orig: &DirPathsOrig, what: &str| {
                let fresh_lower = DirPathsLower::build(idx);
                let fresh_orig = DirPathsOrig::build(idx);
                for id in 0..idx.len() {
                    assert_eq!(
                        lower.0.paths[id], fresh_lower.0.paths[id],
                        "{what}: lower of {id}"
                    );
                    assert_eq!(
                        orig.0.paths[id], fresh_orig.0.paths[id],
                        "{what}: orig of {id}"
                    );
                }
            };

        let mut idx = deep_index();
        let lower = DirPathsLower::build(&idx);
        let orig = DirPathsOrig::build(&idx);

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
        let lower = DirPathsLower::extend_from(&idx, Arc::new(lower));
        let orig = DirPathsOrig::extend_from(&idx, Arc::new(orig));
        assert_same_as_fresh(&idx, &lower, &orig, "append generation");

        // Gen step 2: in-place dir rename — topology bump, extend must
        // rebuild and descendants must reflect the new name.
        let renamed = u16s("renamed_mid");
        idx.rename_dir_in_place(110, &renamed, 109).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let lower = DirPathsLower::extend_from(&idx, Arc::new(lower));
        let orig = DirPathsOrig::extend_from(&idx, Arc::new(orig));
        assert_same_as_fresh(&idx, &lower, &orig, "rename generation");

        // Gen step 3: dir move (reparent) — also a topology bump.
        idx.reparent(500, 100).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let lower = DirPathsLower::extend_from(&idx, Arc::new(lower));
        let orig = DirPathsOrig::extend_from(&idx, Arc::new(orig));
        assert_same_as_fresh(&idx, &lower, &orig, "reparent generation");

        // File-only batches keep the fast path: same topology generation.
        let first_new = idx.len() as u32;
        let f2 = u16s("plain.txt");
        idx.upsert(&raw(503, 100, &f2, false, 1, 4));
        idx.merge_new_into_permutations(first_new);
        let topo_before = idx.dir_topology_generation();
        let lower = DirPathsLower::extend_from(&idx, Arc::new(lower));
        let orig = DirPathsOrig::extend_from(&idx, Arc::new(orig));
        assert_eq!(idx.dir_topology_generation(), topo_before);
        assert_same_as_fresh(&idx, &lower, &orig, "file-only generation");
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

    /// Oracle: an incrementally extended lazy sort permutation equals a
    /// fresh parallel sort byte-for-byte across append/delete generations
    /// (strict total order → the sorted result is unique).
    #[test]
    fn lazy_sort_perms_extend_like_fresh_builds() {
        let mut idx = build_sample();
        let mut size_perm = SortPerm::build(&idx, SortKey::Size);
        let mut mtime_perm = SortPerm::build(&idx, SortKey::Mtime);
        for g in 0..20u64 {
            let first_new = idx.len() as u32;
            let record = 200 + g;
            // Mix sizes across the u32 overflow boundary.
            let size = if g % 4 == 0 { (4u64 << 30) + g } else { g * 37 };
            let name = u16s(&format!("lazy_{g}.bin"));
            // Distinct post-1970 mtimes so the lazy Mtime permutation exercises
            // a real ordering across generations (ADR-0031).
            let mtime = crate::query::dates::FILETIME_UNIX_EPOCH + (g as i64 + 1) * 864_000_000_000;
            idx.upsert(&raw(record, 50, &name, false, size, mtime));
            if g % 3 == 0 {
                idx.delete(200 + g / 2);
            }
            idx.merge_new_into_permutations(first_new);

            size_perm = SortPerm::extend(&idx, size_perm, SortKey::Size);
            mtime_perm = SortPerm::extend(&idx, mtime_perm, SortKey::Mtime);
            assert_eq!(
                size_perm.ids,
                SortPerm::build(&idx, SortKey::Size).ids,
                "size order diverged at generation {g}"
            );
            assert_eq!(
                mtime_perm.ids,
                SortPerm::build(&idx, SortKey::Mtime).ids,
                "mtime order diverged at generation {g}"
            );
        }
    }

    /// The cached lazy permutation survives stat updates as a complete
    /// permutation (stale positions are pinned behavior), and `get`
    /// caches within a generation / extends across one.
    #[test]
    fn size_perm_get_caches_and_stays_complete_under_stat_updates() {
        let mut idx = build_sample();
        let p1 = SizePerm::get(&idx);
        let p2 = SizePerm::get(&idx);
        assert!(Arc::ptr_eq(&p1, &p2), "same generation must cache-hit");
        drop((p1, p2));

        idx.update_stat(100, 999_999, 1).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let p3 = SizePerm::get(&idx);
        let mut seen: Vec<u32> = p3.0.ids.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..idx.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn dir_path_memos_are_lazy_and_accounted_separately() {
        let idx = build_sample();
        // Nothing is cached until a path query builds it — `prewarm` is a
        // no-op since ADR-0032 removed the offset-table cache.
        prewarm(&idx);
        assert!(idx.derived_probe::<DirPathsLower>().is_none());
        assert!(idx.derived_probe::<DirPathsOrig>().is_none());
        assert_eq!(
            derived_cache_bytes(&idx),
            0,
            "no derived caches until a query"
        );

        let _ = idx.cached_derived_or_update(|prev| match prev {
            Some(p) => DirPathsLower::extend_from(&idx, p),
            None => DirPathsLower::build(&idx),
        });
        let with_lower = derived_cache_bytes(&idx);
        assert!(with_lower > 0, "the folded memo joins the accounting");
        assert!(
            idx.derived_probe::<DirPathsOrig>().is_none(),
            "building the folded memo must not drag the orig memo in"
        );

        let _ = idx.cached_derived_or_update(|prev| match prev {
            Some(p) => DirPathsOrig::extend_from(&idx, p),
            None => DirPathsOrig::build(&idx),
        });
        assert!(
            derived_cache_bytes(&idx) > with_lower,
            "orig memo accounted separately"
        );
    }

    #[test]
    fn parent_cycle_attaches_dirs_at_root_instead_of_aborting() {
        // Corrupt USN records can produce a parent cycle (a→b→a). Cycle
        // members must come out root-attached, with paths intact — not
        // abort via a u32::MAX depth poisoning the level-table size.
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (da, db, f) = (u16s("a"), u16s("b"), u16s("f.txt"));
        b.push(raw(10, 5, &da, true, 0, 1));
        b.push(raw(20, 10, &db, true, 0, 2));
        b.push(raw(30, 20, &f, false, 1, 3));
        let mut idx = b.finish();
        idx.reparent(10, 20); // a under b while b is under a — cycle
        let a = idx.entry_by_record(10).unwrap();
        let bb = idx.entry_by_record(20).unwrap();

        let memo = DirPathsLower::build(&idx);
        for d in [a, bb] {
            let lower = memo.0.paths[d as usize]
                .as_deref()
                .expect("cycle members still get a path");
            assert!(lower.ends_with(b"\\"));
        }
    }
}
