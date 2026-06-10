use std::any::Any;
use std::sync::Arc;

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use super::{EntryId, NO_PARENT, SortKey, flags, masked};

pub struct VolumeIndex {
    pub(super) name_pool: Vec<u8>,
    pub(super) lower_pool: Vec<u8>,
    pub(super) name_off: Vec<u32>,
    pub(super) name_len: Vec<u16>,
    pub(super) parent: Vec<EntryId>,
    pub(super) size: Vec<u64>,
    pub(super) mtime: Vec<i64>,
    pub(super) frn: Vec<u64>,
    pub(super) flag: Vec<u8>,
    pub(super) frn_map: FxHashMap<u64, EntryId>,
    pub(super) perm_name: Vec<EntryId>,
    pub(super) perm_size: Vec<EntryId>,
    pub(super) perm_mtime: Vec<EntryId>,
    pub(super) content_generation: u64,
    pub(super) structural_generation: u64,
    pub(super) tombstones: u32,
    /// Query-independent caches derived from index content (dir-path memo,
    /// pool offset table, …) keyed by `content_generation` and value type.
    /// Type-erased so the index stays ignorant of query-module types.
    pub(super) derived_cache: Mutex<Option<(u64, DerivedMap)>>,
}

pub(super) type DerivedMap = FxHashMap<std::any::TypeId, Arc<dyn Any + Send + Sync>>;

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
    pub fn size(&self, id: EntryId) -> u64 {
        self.size[id as usize]
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
        self.frn_map.get(&masked(record_or_frn)).copied()
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
        let key = std::any::TypeId::of::<T>();
        let mut guard = self.derived_cache.lock();
        match guard.as_mut() {
            Some((generation, map)) if *generation == self.content_generation => {
                if let Some(v) = map.get(&key)
                    && let Ok(t) = v.clone().downcast::<T>()
                {
                    return t;
                }
                let t = Arc::new(build());
                map.insert(key, t.clone());
                t
            }
            _ => {
                let t = Arc::new(build());
                let mut map: FxHashMap<std::any::TypeId, Arc<dyn Any + Send + Sync>> =
                    FxHashMap::default();
                map.insert(key, t.clone());
                *guard = Some((self.content_generation, map));
                t
            }
        }
    }

    /// Per-column memory accounting for the perf panel / `fmf stats`.
    /// The map size is an estimate (hashbrown control bytes + slot padding).
    pub fn stats(&self, volume: &str) -> crate::metrics::IndexStats {
        let n = self.len() as u64;
        let offsets = (self.name_off.capacity() * 4 + self.name_len.capacity() * 2) as u64;
        let perms =
            ((self.perm_name.capacity() + self.perm_size.capacity() + self.perm_mtime.capacity())
                * 4) as u64;
        let frn_map = (self.frn_map.capacity() as u64) * 17;
        let mut s = crate::metrics::IndexStats {
            volume: volume.to_string(),
            entries: n,
            live_entries: self.live_len() as u64,
            tombstones: self.tombstones as u64,
            name_pool_bytes: self.name_pool.capacity() as u64,
            lower_pool_bytes: self.lower_pool.capacity() as u64,
            offsets_bytes: offsets,
            parent_bytes: (self.parent.capacity() * 4) as u64,
            size_bytes: (self.size.capacity() * 8) as u64,
            mtime_bytes: (self.mtime.capacity() * 8) as u64,
            frn_bytes: (self.frn.capacity() * 8) as u64,
            flag_bytes: self.flag.capacity() as u64,
            permutations_bytes: perms,
            frn_map_bytes: frn_map,
            content_generation: self.content_generation,
            structural_generation: self.structural_generation,
            ..Default::default()
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
        self.name_pool.shrink_to_fit();
        self.lower_pool.shrink_to_fit();
        self.name_off.shrink_to_fit();
        self.name_len.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.size.shrink_to_fit();
        self.mtime.shrink_to_fit();
        self.frn.shrink_to_fit();
        self.flag.shrink_to_fit();
        self.perm_name.shrink_to_fit();
        self.perm_size.shrink_to_fit();
        self.perm_mtime.shrink_to_fit();
    }

    pub fn permutation(&self, key: SortKey) -> &[EntryId] {
        match key {
            SortKey::Name => &self.perm_name,
            SortKey::Size => &self.perm_size,
            SortKey::Mtime => &self.perm_mtime,
        }
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

    #[inline]
    pub(super) fn cmp_by(&self, key: SortKey, a: EntryId, b: EntryId) -> std::cmp::Ordering {
        match key {
            SortKey::Name => self.lower_name(a).cmp(self.lower_name(b)).then(a.cmp(&b)),
            SortKey::Size => self.size(a).cmp(&self.size(b)).then(a.cmp(&b)),
            SortKey::Mtime => self.mtime(a).cmp(&self.mtime(b)).then(a.cmp(&b)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn permutations_are_sorted() {
        let idx = build_sample();
        let by_name: Vec<&[u8]> = idx
            .permutation(SortKey::Name)
            .iter()
            .map(|&id| idx.lower_name(id))
            .collect();
        let mut expect = by_name.clone();
        expect.sort();
        assert_eq!(by_name, expect);

        let sizes: Vec<u64> = idx
            .permutation(SortKey::Size)
            .iter()
            .map(|&id| idx.size(id))
            .collect();
        assert!(sizes.is_sorted());
    }
}
