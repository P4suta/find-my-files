use std::any::Any;
use std::sync::Arc;

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use super::frn::FrnIndex;
use super::{EntryId, NO_PARENT, SortKey, flags, masked};

pub struct VolumeIndex {
    pub(super) name_pool: Vec<u8>,
    pub(super) lower_pool: Vec<u8>,
    pub(super) name_off: Vec<u32>,
    pub(super) name_len: Vec<u16>,
    pub(super) parent: Vec<EntryId>,
    /// File sizes < u32::MAX, 4 bytes per entry; u32::MAX is the sentinel
    /// for the overflow map (≥4GiB files — 10 of 1.27M on the measured
    /// real C:, docs/RESEARCH.md). Read through [`VolumeIndex::size`].
    pub(super) size_lo: Vec<u32>,
    pub(super) size_ovf: FxHashMap<EntryId, u64>,
    pub(super) mtime: Vec<i64>,
    pub(super) frn: Vec<u64>,
    pub(super) flag: Vec<u8>,
    pub(super) frn_index: FrnIndex,
    /// The one always-maintained permutation: name order is the default
    /// sort and the merge target of every USN batch. Size/mtime orders are
    /// lazily derived caches (query::memo::{SizePerm, MtimePerm}) — built on
    /// the first sorted query, extended per generation, never persisted.
    pub(super) perm_name: Vec<EntryId>,
    pub(super) content_generation: u64,
    pub(super) structural_generation: u64,
    /// Bumped whenever an existing directory's name or parent changes —
    /// the two mutations that invalidate memoized descendant paths in ways
    /// an append-only extension cannot express. Plain appends/deletes/stat
    /// updates leave it untouched.
    pub(super) dir_topology_generation: u64,
    pub(super) tombstones: u32,
    /// Abandoned name bytes *per pool* (tombstoned rows and in-place dir
    /// renames leave their old bytes behind; ×2 pools is the reclaimable
    /// garbage). Compaction-trigger input. Not persisted — recomputed from
    /// tombstones on restore, so rename gaps make it a lower bound there.
    pub(super) dead_name_bytes: u64,
    /// Query-independent caches derived from index content (dir-path memo,
    /// pool offset table, …) keyed by `content_generation` and value type.
    /// Type-erased so the index stays ignorant of query-module types.
    pub(super) derived_cache: Mutex<Option<DerivedCache>>,
}

pub(super) type DerivedMap = FxHashMap<std::any::TypeId, Arc<dyn Any + Send + Sync>>;

/// The previous generation's values stick around (`prev`) so incremental
/// builders can extend them instead of starting over; a value is consumed
/// (removed) the first time its type resolves under the new generation, and
/// anything never consumed drops on the following generation change.
pub(super) struct DerivedCache {
    generation: u64,
    current: DerivedMap,
    prev: DerivedMap,
}

impl VolumeIndex {
    pub fn len(&self) -> usize {
        self.name_off.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn live_len(&self) -> usize {
        self.len() - self.tombstones as usize
    }

    pub const ROOT: EntryId = 0;

    #[inline]
    pub fn name(&self, id: EntryId) -> &[u8] {
        let off = self.name_off[id as usize] as usize;
        &self.name_pool[off..off + self.name_len[id as usize] as usize]
    }

    #[inline]
    pub fn lower_name(&self, id: EntryId) -> &[u8] {
        let off = self.name_off[id as usize] as usize;
        &self.lower_pool[off..off + self.name_len[id as usize] as usize]
    }

    #[inline]
    pub fn is_live(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::TOMBSTONE == 0
    }

    /// Hidden/system (or under such a branch) — skipped by default queries.
    #[inline]
    pub fn is_excluded(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::EXCLUDED != 0
    }

    #[inline]
    pub fn is_dir(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::IS_DIR != 0
    }

    #[inline]
    pub fn is_reparse(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::REPARSE != 0
    }

    #[inline]
    pub fn size(&self, id: EntryId) -> u64 {
        match self.size_lo[id as usize] {
            u32::MAX => self.size_ovf[&id],
            v => v as u64,
        }
    }

    /// The single write path for sizes — keeps the column and the overflow
    /// map consistent in both directions (a file can shrink back under the
    /// sentinel).
    pub(super) fn set_size(&mut self, id: EntryId, v: u64) {
        if v >= u32::MAX as u64 {
            self.size_lo[id as usize] = u32::MAX;
            self.size_ovf.insert(id, v);
        } else {
            self.size_lo[id as usize] = v as u32;
            self.size_ovf.remove(&id);
        }
    }

    /// Append form of [`Self::set_size`] (column construction).
    pub(super) fn push_size(&mut self, v: u64) {
        if v >= u32::MAX as u64 {
            let id = self.size_lo.len() as EntryId;
            self.size_lo.push(u32::MAX);
            self.size_ovf.insert(id, v);
        } else {
            self.size_lo.push(v as u32);
        }
    }

    #[inline]
    pub fn mtime(&self, id: EntryId) -> i64 {
        self.mtime[id as usize]
    }

    #[inline]
    pub fn parent(&self, id: EntryId) -> EntryId {
        self.parent[id as usize]
    }

    #[inline]
    pub fn frn(&self, id: EntryId) -> u64 {
        self.frn[id as usize]
    }

    pub fn entry_by_record(&self, record_or_frn: u64) -> Option<EntryId> {
        self.frn_index
            .lookup(masked(record_or_frn), &self.frn, &self.flag)
    }

    // Raw pool access for the pool-scan query kernel (same crate only).
    #[inline]
    pub(crate) fn name_off_of(&self, id: EntryId) -> u32 {
        self.name_off[id as usize]
    }

    #[inline]
    pub(crate) fn name_pool_bytes(&self) -> &[u8] {
        &self.name_pool
    }

    #[inline]
    pub(crate) fn lower_pool_bytes(&self) -> &[u8] {
        &self.lower_pool
    }

    pub fn content_generation(&self) -> u64 {
        self.content_generation
    }

    pub fn structural_generation(&self) -> u64 {
        self.structural_generation
    }

    pub(crate) fn dir_topology_generation(&self) -> u64 {
        self.dir_topology_generation
    }

    /// Carry the structural generation across a rebuild: a freshly built
    /// index replacing one whose generation was `prev` must read as strictly
    /// newer, so open result handles go hard-stale (docs/ARCHITECTURE.md,
    /// generation 2層). Compaction (M2) will reuse this.
    pub(crate) fn bump_structural_from(&mut self, prev: u64) {
        self.structural_generation = prev + 1;
    }

    /// Return the cached content-derived value of type `T`, rebuilding it
    /// with `build` when the content generation moved. All cached types are
    /// invalidated together on a generation change.
    pub(crate) fn cached_derived<T, F>(&self, build: F) -> Arc<T>
    where
        T: Any + Send + Sync,
        F: FnOnce() -> T,
    {
        self.with_derived(|_| build())
    }

    /// Like [`Self::cached_derived`], but on a generation change `build`
    /// receives the previous generation's value so it can extend it
    /// incrementally instead of rebuilding from scratch.
    pub(crate) fn cached_derived_or_update<T, F>(&self, build: F) -> Arc<T>
    where
        T: Any + Send + Sync,
        F: FnOnce(Option<Arc<T>>) -> T,
    {
        self.with_derived(build)
    }

    /// Read-only probe of the current generation's cached `T` — never
    /// builds. For memory accounting (`IndexStats.derived_cache_bytes`).
    pub(crate) fn derived_probe<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let guard = self.derived_cache.lock();
        let cache = guard.as_ref()?;
        if cache.generation != self.content_generation {
            return None;
        }
        cache
            .current
            .get(&std::any::TypeId::of::<T>())?
            .clone()
            .downcast::<T>()
            .ok()
    }

    fn with_derived<T, F>(&self, build: F) -> Arc<T>
    where
        T: Any + Send + Sync,
        F: FnOnce(Option<Arc<T>>) -> T,
    {
        let key = std::any::TypeId::of::<T>();
        let mut guard = self.derived_cache.lock();
        let cache = guard.get_or_insert_with(|| DerivedCache {
            generation: self.content_generation,
            current: DerivedMap::default(),
            prev: DerivedMap::default(),
        });
        if cache.generation != self.content_generation {
            cache.prev = std::mem::take(&mut cache.current);
            cache.generation = self.content_generation;
        }
        if let Some(v) = cache.current.get(&key)
            && let Ok(t) = v.clone().downcast::<T>()
        {
            return t;
        }
        let previous = cache.prev.remove(&key).and_then(|v| v.downcast::<T>().ok());
        let t = Arc::new(build(previous));
        cache.current.insert(key, t.clone());
        t
    }

    /// Per-column memory accounting for the perf panel / `fmf stats`.
    /// The map size is an estimate (hashbrown control bytes + slot padding).
    pub fn stats(&self, volume: &str) -> crate::metrics::IndexStats {
        let n = self.len() as u64;
        let offsets = (self.name_off.capacity() * 4 + self.name_len.capacity() * 2) as u64;
        // perm_name only — the lazy size/mtime permutations are accounted
        // with the derived caches (`derived_cache_bytes`).
        let perms = (self.perm_name.capacity() * 4) as u64;
        // Field name kept for FFI/JSON compatibility; the structure is the
        // sorted FRN permutation these days (index/frn.rs).
        let frn_map = self.frn_index.bytes();
        let mut s = crate::metrics::IndexStats {
            volume: volume.to_string(),
            entries: n,
            live_entries: self.live_len() as u64,
            tombstones: self.tombstones as u64,
            name_pool_bytes: self.name_pool.capacity() as u64,
            lower_pool_bytes: self.lower_pool.capacity() as u64,
            offsets_bytes: offsets,
            parent_bytes: (self.parent.capacity() * 4) as u64,
            // Column + the overflow map (hashbrown: (K,V) slot + 1 control
            // byte per capacity slot — an estimate, the map holds ~10
            // entries on a real volume).
            size_bytes: (self.size_lo.capacity() * 4
                + self.size_ovf.capacity() * (std::mem::size_of::<(EntryId, u64)>() + 1))
                as u64,
            mtime_bytes: (self.mtime.capacity() * 8) as u64,
            frn_bytes: (self.frn.capacity() * 8) as u64,
            flag_bytes: self.flag.capacity() as u64,
            permutations_bytes: perms,
            frn_map_bytes: frn_map,
            dead_name_bytes: self.dead_name_bytes,
            content_generation: self.content_generation,
            structural_generation: self.structural_generation,
            ..Default::default()
        };
        let pool_bytes = s.name_pool_bytes + s.lower_pool_bytes;
        s.pool_garbage_ratio = if pool_bytes > 0 {
            (self.dead_name_bytes * 2) as f64 / pool_bytes as f64
        } else {
            0.0
        };
        s.total_bytes = s.name_pool_bytes
            + s.lower_pool_bytes
            + s.offsets_bytes
            + s.parent_bytes
            + s.size_bytes
            + s.mtime_bytes
            + s.frn_bytes
            + s.flag_bytes
            + s.permutations_bytes
            + s.frn_map_bytes;
        s.bytes_per_entry = if n > 0 {
            s.total_bytes as f64 / n as f64
        } else {
            0.0
        };
        s
    }

    /// Trim over-allocated columns after a bulk build.
    pub fn shrink_to_fit(&mut self) {
        self.frn_index.shrink_to_fit();
        self.name_pool.shrink_to_fit();
        self.lower_pool.shrink_to_fit();
        self.name_off.shrink_to_fit();
        self.name_len.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.size_lo.shrink_to_fit();
        self.size_ovf.shrink_to_fit();
        self.mtime.shrink_to_fit();
        self.frn.shrink_to_fit();
        self.flag.shrink_to_fit();
        self.perm_name.shrink_to_fit();
    }

    pub fn name_permutation(&self) -> &[EntryId] {
        &self.perm_name
    }

    /// Append the full WTF-8 path of `id` ("C:\dir\file.txt") to `out`.
    /// Built lazily from the parent chain — paths are never stored.
    pub fn append_path(&self, id: EntryId, out: &mut Vec<u8>) {
        self.append_parent_path(id, out);
        if id != Self::ROOT {
            out.extend_from_slice(self.name(id));
        }
    }

    /// Append the path of `id`'s parent directory, including a trailing `\`.
    pub fn append_parent_path(&self, id: EntryId, out: &mut Vec<u8>) {
        let mut chain = [0u32; 128];
        let mut depth = 0;
        let mut cur = if id == Self::ROOT {
            NO_PARENT
        } else {
            self.parent(id)
        };
        while cur != NO_PARENT && depth < chain.len() {
            chain[depth] = cur;
            depth += 1;
            cur = if cur == Self::ROOT {
                NO_PARENT
            } else {
                self.parent(cur)
            };
        }
        for &c in chain[..depth].iter().rev() {
            out.extend_from_slice(self.name(c));
            out.push(b'\\');
        }
    }

    pub fn tombstone_ratio(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.tombstones as f64 / self.len() as f64
        }
    }

    /// The one definition of each sort key's strict total order (id
    /// tie-break) — `pub(crate)` so the lazy permutation caches in the
    /// query layer sort by exactly the same order the merge maintains.
    #[inline]
    pub(crate) fn cmp_by(&self, key: SortKey, a: EntryId, b: EntryId) -> std::cmp::Ordering {
        self.sort_columns().cmp_by(key, a, b)
    }

    pub(super) fn sort_columns(&self) -> SortColumns<'_> {
        SortColumns {
            lower_pool: &self.lower_pool,
            name_off: &self.name_off,
            name_len: &self.name_len,
            size_lo: &self.size_lo,
            size_ovf: &self.size_ovf,
            mtime: &self.mtime,
        }
    }
}

/// Borrowed view of the sort-key columns, so permutation maintenance can
/// hold `&mut` permutation arrays while comparing through the one
/// definition of each key's order (a drifting duplicate of `cmp_by` would
/// silently corrupt the merge).
pub(super) struct SortColumns<'a> {
    lower_pool: &'a [u8],
    name_off: &'a [u32],
    name_len: &'a [u16],
    size_lo: &'a [u32],
    size_ovf: &'a FxHashMap<EntryId, u64>,
    mtime: &'a [i64],
}

impl<'a> SortColumns<'a> {
    pub(super) fn new(
        lower_pool: &'a [u8],
        name_off: &'a [u32],
        name_len: &'a [u16],
        size_lo: &'a [u32],
        size_ovf: &'a FxHashMap<EntryId, u64>,
        mtime: &'a [i64],
    ) -> Self {
        Self {
            lower_pool,
            name_off,
            name_len,
            size_lo,
            size_ovf,
            mtime,
        }
    }

    #[inline]
    fn lower_name(&self, id: EntryId) -> &[u8] {
        let off = self.name_off[id as usize] as usize;
        &self.lower_pool[off..off + self.name_len[id as usize] as usize]
    }

    #[inline]
    fn size_of(&self, id: EntryId) -> u64 {
        match self.size_lo[id as usize] {
            u32::MAX => self.size_ovf[&id],
            v => v as u64,
        }
    }

    /// Strict total order (id tie-break): no two distinct ids compare equal,
    /// which is what makes merged permutations byte-deterministic.
    #[inline]
    pub(super) fn cmp_by(&self, key: SortKey, a: EntryId, b: EntryId) -> std::cmp::Ordering {
        match key {
            SortKey::Name => self.lower_name(a).cmp(self.lower_name(b)).then(a.cmp(&b)),
            SortKey::Size => self.size_of(a).cmp(&self.size_of(b)).then(a.cmp(&b)),
            SortKey::Mtime => self.mtime[a as usize]
                .cmp(&self.mtime[b as usize])
                .then(a.cmp(&b)),
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::index::testutil::build_sample;

    #[test]
    fn full_path_builds_lazily() {
        let idx = build_sample();
        let note = idx.entry_by_record(100).unwrap();
        let mut p = Vec::new();
        idx.append_path(note, &mut p);
        assert_eq!(p, b"C:\\docs\\Note.TXT");

        let mut pp = Vec::new();
        idx.append_parent_path(note, &mut pp);
        assert_eq!(pp, b"C:\\docs\\");
    }

    #[test]
    fn name_permutation_is_sorted() {
        let idx = build_sample();
        let by_name: Vec<&[u8]> = idx
            .name_permutation()
            .iter()
            .map(|&id| idx.lower_name(id))
            .collect();
        let mut expect = by_name.clone();
        expect.sort();
        assert_eq!(by_name, expect);
    }
}
