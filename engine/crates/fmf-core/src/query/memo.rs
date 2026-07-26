use rayon::prelude::*;

use crate::engine::{EngineError, QueryCancellation};
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
    #[cfg(test)]
    pub(super) fn get(idx: &VolumeIndex) -> std::sync::Arc<Self> {
        Self::get_cancellable(idx, &QueryCancellation::new())
            .expect("fresh cancellation token cannot cancel")
    }

    pub(super) fn get_cancellable(
        idx: &VolumeIndex,
        cancellation: &QueryCancellation,
    ) -> Result<std::sync::Arc<Self>, EngineError> {
        idx.cached_derived_or_try_update(|prev| {
            Ok(match prev {
                Some(p) => Self(SortPerm::extend_cancellable(
                    idx,
                    take_perm(p, |m: &Self| &m.0),
                    SortKey::Size,
                    cancellation,
                )?),
                None => Self(SortPerm::build_cancellable(
                    idx,
                    SortKey::Size,
                    cancellation,
                )?),
            })
        })
    }
}

impl MtimePerm {
    pub(super) fn get_cancellable(
        idx: &VolumeIndex,
        cancellation: &QueryCancellation,
    ) -> Result<std::sync::Arc<Self>, EngineError> {
        idx.cached_derived_or_try_update(|prev| {
            Ok(match prev {
                Some(p) => Self(SortPerm::extend_cancellable(
                    idx,
                    take_perm(p, |m: &Self| &m.0),
                    SortKey::Mtime,
                    cancellation,
                )?),
                None => Self(SortPerm::build_cancellable(
                    idx,
                    SortKey::Mtime,
                    cancellation,
                )?),
            })
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
    #[cfg(test)]
    fn build(idx: &VolumeIndex, key: SortKey) -> Self {
        Self::build_cancellable(idx, key, &QueryCancellation::new())
            .expect("fresh cancellation token cannot cancel")
    }

    fn build_cancellable(
        idx: &VolumeIndex,
        key: SortKey,
        cancellation: &QueryCancellation,
    ) -> Result<Self, EngineError> {
        let ids = cancellable_sort((0..idx.len() as u32).collect(), idx, key, cancellation)?;
        Ok(Self {
            ids,
            covers: idx.len() as u32,
        })
    }

    #[cfg(test)]
    fn extend(idx: &VolumeIndex, perm: Self, key: SortKey) -> Self {
        Self::extend_cancellable(idx, perm, key, &QueryCancellation::new())
            .expect("fresh cancellation token cannot cancel")
    }

    fn extend_cancellable(
        idx: &VolumeIndex,
        mut perm: Self,
        key: SortKey,
        cancellation: &QueryCancellation,
    ) -> Result<Self, EngineError> {
        cancellation.check()?;
        let n = idx.len() as u32;
        // Entries are append-only within a structural generation — a
        // regressed watermark means the cache got crossed with a different
        // index. Rebuilding recovers; the fact must not vanish.
        if perm.covers > n {
            crate::degrade!(
                crate::metrics::LAZY_PERM_REBUILD_FALLBACKS,
                covers = perm.covers,
                entries = n,
                "lazy sort permutation watermark regressed — falling back to a full rebuild"
            );
            return Self::build_cancellable(idx, key, cancellation);
        }
        let batch = cancellable_sort((perm.covers..n).collect(), idx, key, cancellation)?;
        if !batch.is_empty() {
            perm.ids = cancellable_merge(&perm.ids, &batch, idx, key, cancellation)?;
        }
        perm.covers = n;
        Ok(perm)
    }
}

/// Sort in bounded independently-cancellable runs, then parallel merge
/// passes. A cancellation waits for at most one small run sort or 1024 merge
/// comparisons; no million-entry `par_sort` continues invisibly.
fn cancellable_sort(
    mut ids: Vec<EntryId>,
    idx: &VolumeIndex,
    key: SortKey,
    cancellation: &QueryCancellation,
) -> Result<Vec<EntryId>, EngineError> {
    const RUN: usize = 4096;
    cancellation.check()?;
    ids.par_chunks_mut(RUN).try_for_each(|chunk| {
        cancellation.check()?;
        chunk.sort_unstable_by(|&a, &b| idx.cmp_by(key, a, b));
        cancellation.check()
    })?;

    let mut source = ids;
    let mut destination = vec![0; source.len()];
    let mut width = RUN;
    while width < source.len() {
        cancellation.check()?;
        let pair_width = width.saturating_mul(2);
        destination
            .par_chunks_mut(pair_width)
            .enumerate()
            .try_for_each(|(pair, out)| {
                cancellation.check()?;
                let start = pair * pair_width;
                let middle = (start + width).min(source.len());
                let end = (start + pair_width).min(source.len());
                let (left, right) = (&source[start..middle], &source[middle..end]);
                let (mut l, mut r) = (0usize, 0usize);
                for (position, target) in out.iter_mut().enumerate() {
                    if position.is_multiple_of(1024) {
                        cancellation.check()?;
                    }
                    let take_left = r == right.len()
                        || (l < left.len()
                            && idx.cmp_by(key, left[l], right[r]) != std::cmp::Ordering::Greater);
                    *target = if take_left {
                        let value = left[l];
                        l += 1;
                        value
                    } else {
                        let value = right[r];
                        r += 1;
                        value
                    };
                }
                Ok::<(), EngineError>(())
            })?;
        std::mem::swap(&mut source, &mut destination);
        width = pair_width;
    }
    cancellation.check()?;
    Ok(source)
}

fn cancellable_merge(
    left: &[EntryId],
    right: &[EntryId],
    idx: &VolumeIndex,
    key: SortKey,
    cancellation: &QueryCancellation,
) -> Result<Vec<EntryId>, EngineError> {
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut l, mut r) = (0usize, 0usize);
    while l < left.len() || r < right.len() {
        if out.len().is_multiple_of(1024) {
            cancellation.check()?;
        }
        let take_left = r == right.len()
            || (l < left.len()
                && idx.cmp_by(key, left[l], right[r]) != std::cmp::Ordering::Greater);
        if take_left {
            out.push(left[l]);
            l += 1;
        } else {
            out.push(right[r]);
            r += 1;
        }
    }
    cancellation.check()?;
    Ok(out)
}

// ── Dir-path memo (generation-cached, one per name pool) ────────────────

/// Compact, sanitized parent topology for path matching.
///
/// Full path strings are deliberately not cached: a chain of `n` one-byte
/// directory names contains Θ(n²) prefix bytes. The index already owns every
/// name once, so this cache stores only one parent id per entry (Θ(n)); each
/// worker materializes the one path it is currently evaluating into reusable
/// scratch.
pub(super) struct DirTopology {
    parents: Vec<EntryId>,
    /// Entries `[0, covers_entries)` have a sanitized parent.
    covers_entries: usize,
    /// The dir-topology generation this cache is valid for.
    topo_generation: u64,
}

impl DirTopology {
    #[cfg(test)]
    fn build(idx: &VolumeIndex) -> Self {
        Self::build_cancellable(idx, &QueryCancellation::new())
            .expect("fresh cancellation token cannot cancel")
    }

    #[cfg(test)]
    fn extend_from(idx: &VolumeIndex, prev: std::sync::Arc<Self>) -> Self {
        Self::extend_from_cancellable(idx, prev, &QueryCancellation::new())
            .expect("fresh cancellation token cannot cancel")
    }

    pub(super) fn extend_from_cancellable(
        idx: &VolumeIndex,
        prev: std::sync::Arc<Self>,
        cancellation: &QueryCancellation,
    ) -> Result<Self, EngineError> {
        let mut topology = match std::sync::Arc::try_unwrap(prev) {
            Ok(owned) => owned,
            Err(shared) => Self {
                parents: shared.parents.clone(),
                covers_entries: shared.covers_entries,
                topo_generation: shared.topo_generation,
            },
        };
        cancellation.check()?;
        let n = idx.len();
        if topology.topo_generation != idx.dir_topology_generation() || topology.covers_entries > n
        {
            return Self::build_cancellable(idx, cancellation);
        }

        topology.parents.resize(n, VolumeIndex::ROOT);
        for id in topology.covers_entries as u32..n as u32 {
            if (id as usize).is_multiple_of(1024) {
                cancellation.check()?;
            }
            topology.parents[id as usize] =
                Self::valid_parent(idx, id).unwrap_or(VolumeIndex::ROOT);
        }
        topology.covers_entries = n;
        cancellation.check()?;
        Ok(topology)
    }

    fn valid_parent(idx: &VolumeIndex, id: EntryId) -> Option<EntryId> {
        if id == VolumeIndex::ROOT {
            return Some(VolumeIndex::ROOT);
        }
        let parent = idx.parent(id);
        ((parent as usize) < idx.len() && idx.is_dir(parent)).then_some(parent)
    }

    /// Build an acyclic parent forest in linear space. Valid trees retain
    /// their exact parents at any depth; malformed cycles or non-directory
    /// parents are detached to the root instead of poisoning every descendant.
    pub(super) fn build_cancellable(
        idx: &VolumeIndex,
        cancellation: &QueryCancellation,
    ) -> Result<Self, EngineError> {
        const UNSEEN: u8 = 0;
        const VISITING: u8 = 1;
        const DONE: u8 = 2;

        cancellation.check()?;
        let n = idx.len();
        let mut parents = vec![VolumeIndex::ROOT; n];
        let mut state = vec![UNSEEN; n];
        state[VolumeIndex::ROOT as usize] = DONE;
        let mut stack = Vec::<EntryId>::new();
        let mut cycle_members = 0u64;
        let mut steps = 0usize;

        for start in 1..n as u32 {
            if !idx.is_dir(start) || state[start as usize] == DONE {
                continue;
            }
            stack.clear();
            let mut current = start;
            loop {
                if steps.is_multiple_of(1024) {
                    cancellation.check()?;
                }
                steps += 1;

                let current_index = current as usize;
                if current_index >= n || !idx.is_dir(current) {
                    break;
                }
                match state[current_index] {
                    UNSEEN => {
                        state[current_index] = VISITING;
                        stack.push(current);
                        let Some(parent) = Self::valid_parent(idx, current) else {
                            break;
                        };
                        current = parent;
                    }
                    VISITING => {
                        let Some(cycle_start) = stack.iter().position(|&entry| entry == current)
                        else {
                            // A VISITING node must belong to this walk because
                            // every prior walk drains its stack to DONE. Treat
                            // a violated cache invariant as a malformed parent
                            // and detach this walk instead of widening a cycle.
                            break;
                        };
                        for entry in stack.split_off(cycle_start) {
                            parents[entry as usize] = VolumeIndex::ROOT;
                            state[entry as usize] = DONE;
                            cycle_members += 1;
                        }
                        break;
                    }
                    DONE => break,
                    _ => unreachable!("directory visitation state is internal"),
                }
            }

            while let Some(entry) = stack.pop() {
                let parent = Self::valid_parent(idx, entry)
                    .filter(|&parent| state[parent as usize] == DONE)
                    .unwrap_or(VolumeIndex::ROOT);
                parents[entry as usize] = parent;
                state[entry as usize] = DONE;
            }
        }

        // Files cannot participate in a parent cycle. Point each at a
        // resolved directory, or at the root for malformed input.
        for id in 1..n as u32 {
            if (id as usize).is_multiple_of(1024) {
                cancellation.check()?;
            }
            if !idx.is_dir(id) {
                parents[id as usize] = Self::valid_parent(idx, id)
                    .filter(|&parent| state[parent as usize] == DONE)
                    .unwrap_or(VolumeIndex::ROOT);
            }
        }

        if cycle_members > 0 {
            tracing::warn!(
                cycle_members,
                "corrupt parent cycle while building path topology — cycle members attached at root"
            );
        }
        cancellation.check()?;
        Ok(Self {
            parents,
            covers_entries: n,
            topo_generation: idx.dir_topology_generation(),
        })
    }

    /// Append the sanitized parent path of `id`, including the volume root and
    /// trailing separators. `chain` is caller-owned scratch reused per entry.
    fn append_parent_path(
        &self,
        idx: &VolumeIndex,
        id: EntryId,
        folded: bool,
        out: &mut Vec<u8>,
        chain: &mut Vec<EntryId>,
    ) {
        chain.clear();
        let mut current = self.parents[id as usize];
        let mut remaining = self.parents.len();
        loop {
            chain.push(current);
            if current == VolumeIndex::ROOT {
                break;
            }
            current = self.parents[current as usize];
            remaining -= 1;
            if remaining == 0 {
                // `build_cancellable` guarantees an acyclic forest. Keep this
                // fail-closed guard so corrupted cached state cannot loop.
                chain.push(VolumeIndex::ROOT);
                break;
            }
        }
        for &directory in chain.iter().rev() {
            let name = if folded {
                idx.lower_name(directory)
            } else {
                idx.name(directory)
            };
            out.extend_from_slice(name);
            out.push(b'\\');
        }
    }

    const fn bytes(&self) -> u64 {
        (self.parents.capacity() * std::mem::size_of::<EntryId>()) as u64
    }
}

/// The compact path topology one query execution may read. `None` means the
/// compiled query contains no path matcher and pays no cache cost.
#[derive(Default)]
pub(super) struct PathMemos {
    pub(super) topology: Option<std::sync::Arc<DirTopology>>,
}

impl PathMemos {
    #[inline]
    pub(super) fn append_lower_parent(
        &self,
        idx: &VolumeIndex,
        id: EntryId,
        out: &mut Vec<u8>,
        chain: &mut Vec<EntryId>,
    ) {
        self.topology
            .as_ref()
            .expect("compiled path query builds topology")
            .append_parent_path(idx, id, true, out, chain);
    }

    #[inline]
    pub(super) fn append_orig_parent(
        &self,
        idx: &VolumeIndex,
        id: EntryId,
        out: &mut Vec<u8>,
        chain: &mut Vec<EntryId>,
    ) {
        self.topology
            .as_ref()
            .expect("compiled path query builds topology")
            .append_parent_path(idx, id, false, out, chain);
    }
}

/// Build the pool offset table ahead of the first query — the engine calls
/// this once a volume turns Ready so no keystroke pays the cold cost.
///
/// Prewarm derived caches at Ready. A no-op since ADR-0032 removed the
/// offset-table cache (the name dictionary is resident from build/restore):
/// the lazy sort and compact path topology are intentionally built on demand —
/// most sessions never sort by size/mtime or issue a path query.
pub const fn prewarm(_idx: &VolumeIndex) {}

/// Bytes currently held by this index's derived caches (dir-path memos and
/// the lazy sort permutations), for the RAM accounting in `IndexStats`.
/// Probes only — never builds.
pub fn derived_cache_bytes(idx: &VolumeIndex) -> u64 {
    let mut total = 0u64;
    if let Some(topology) = idx.derived_probe::<DirTopology>() {
        total += topology.bytes();
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

    /// A 60-deep directory chain plus a multibyte directory and files.
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

    fn topology_path(
        topology: &DirTopology,
        idx: &VolumeIndex,
        id: EntryId,
        folded: bool,
    ) -> Vec<u8> {
        let mut path = Vec::new();
        let mut chain = Vec::new();
        if id != VolumeIndex::ROOT {
            topology.append_parent_path(idx, id, folded, &mut path, &mut chain);
        }
        path.extend_from_slice(if folded {
            idx.lower_name(id)
        } else {
            idx.name(id)
        });
        path.push(b'\\');
        path
    }

    fn assert_memo_matches_oracle(idx: &VolumeIndex) {
        let topology = DirTopology::build(idx);
        for id in 0..idx.len() as u32 {
            if idx.is_dir(id) {
                let (lower, orig) = oracle_paths(idx, id);
                assert_eq!(
                    topology_path(&topology, idx, id, true),
                    lower,
                    "lower path of dir {id}"
                );
                assert_eq!(
                    topology_path(&topology, idx, id, false),
                    orig,
                    "orig path of dir {id}"
                );
                // And the oracle itself agrees with the core path builder.
                // (append_path skips the root's own name — the volume label
                // is rendered by callers via name(); see engine/results.rs —
                // so the cross-check only applies below the root.)
                if id != VolumeIndex::ROOT {
                    let mut ap = Vec::new();
                    idx.append_path(id, &mut ap).unwrap();
                    ap.push(b'\\');
                    assert_eq!(orig, ap, "append_path oracle of dir {id}");
                }
            }
        }
    }

    #[test]
    fn dir_paths_match_append_path_oracle() {
        assert_memo_matches_oracle(&deep_index());
    }

    #[test]
    fn very_deep_chain_uses_linear_cache_and_keeps_the_complete_path() {
        const DEPTH: u64 = 10_000;
        let mut builder = VolumeIndexBuilder::new("C:", 5);
        let component = u16s("x");
        for offset in 0..DEPTH {
            let record = 10 + offset;
            let parent = if offset == 0 { 5 } else { record - 1 };
            builder.push(raw(record, parent, &component, true, 0, offset as i64));
        }
        let idx = builder.finish();
        let topology = DirTopology::build(&idx);
        let deepest = idx.entry_by_record(9 + DEPTH).unwrap();
        let path = topology_path(&topology, &idx, deepest, false);

        assert_eq!(path.len(), b"C:\\".len() + DEPTH as usize * b"x\\".len());
        assert_eq!(topology.parents.len(), idx.len());
        assert!(
            topology.bytes() <= (idx.len() * 2 * std::mem::size_of::<EntryId>()) as u64,
            "cache must stay O(entries), not O(total prefix bytes)"
        );
    }

    /// Oracle: an incrementally extended topology must equal a fresh build —
    /// across appended entries (extend fast path) and dir renames / moves
    /// (topology bump → internal full rebuild).
    #[test]
    fn extended_dir_paths_match_fresh_build() {
        let assert_same_as_fresh = |idx: &VolumeIndex, topology: &DirTopology, what: &str| {
            let fresh = DirTopology::build(idx);
            assert_eq!(topology.parents, fresh.parents, "{what}");
        };

        let mut idx = deep_index();
        let topology = DirTopology::build(&idx);

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
        let topology = DirTopology::extend_from(&idx, Arc::new(topology));
        assert_same_as_fresh(&idx, &topology, "append generation");

        // Gen step 2: in-place dir rename — topology bump, extend must
        // rebuild and descendants must reflect the new name.
        let renamed = u16s("renamed_mid");
        idx.rename_dir_in_place(110, &renamed, 109).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let topology = DirTopology::extend_from(&idx, Arc::new(topology));
        assert_same_as_fresh(&idx, &topology, "rename generation");

        // Gen step 3: dir move (reparent) — also a topology bump.
        idx.reparent(500, 100).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);
        let topology = DirTopology::extend_from(&idx, Arc::new(topology));
        assert_same_as_fresh(&idx, &topology, "reparent generation");

        // File-only batches keep the fast path: same topology generation.
        let first_new = idx.len() as u32;
        let f2 = u16s("plain.txt");
        idx.upsert(&raw(503, 100, &f2, false, 1, 4));
        idx.merge_new_into_permutations(first_new);
        let topo_before = idx.dir_topology_generation();
        let topology = DirTopology::extend_from(&idx, Arc::new(topology));
        assert_eq!(idx.dir_topology_generation(), topo_before);
        assert_same_as_fresh(&idx, &topology, "file-only generation");
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
    fn dir_topology_is_lazy_and_accounted_once() {
        let idx = build_sample();
        // Nothing is cached until a path query builds it — `prewarm` is a
        // no-op since ADR-0032 removed the offset-table cache.
        prewarm(&idx);
        assert!(idx.derived_probe::<DirTopology>().is_none());
        assert_eq!(
            derived_cache_bytes(&idx),
            0,
            "no derived caches until a query"
        );

        let _ = idx.cached_derived_or_update(|prev| match prev {
            Some(p) => DirTopology::extend_from(&idx, p),
            None => DirTopology::build(&idx),
        });
        assert!(
            derived_cache_bytes(&idx) > 0,
            "the topology joins derived-cache accounting"
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
        idx.reparent(10, 20).unwrap(); // a under b while b is under a — cycle
        let a = idx.entry_by_record(10).unwrap();
        let bb = idx.entry_by_record(20).unwrap();
        let file = idx.entry_by_record(30).unwrap();

        let topology = DirTopology::build(&idx);
        assert_eq!(topology_path(&topology, &idx, a, true), b"c:\\a\\");
        assert_eq!(topology_path(&topology, &idx, bb, true), b"c:\\b\\");
        assert_eq!(
            topology_path(&topology, &idx, file, true),
            b"c:\\b\\f.txt\\"
        );
    }
}
