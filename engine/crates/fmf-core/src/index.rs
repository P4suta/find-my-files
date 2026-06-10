//! In-memory per-volume index: struct-of-arrays, two string pools with shared
//! offsets, FRN map, and pre-sorted permutations for instant sorting
//! (docs/ARCHITECTURE.md).
//!
//! Mutation model (keeps the permutation arrays merge-only):
//! - create  → append entry + merge into permutations
//! - delete  → tombstone flag only
//! - rename  → tombstone old + append new entry with the same FRN
//! - move    → rewrite `parent` only (no permutation depends on the path)
//!
//! Tombstones accumulate until compaction (M2), which bumps
//! `structural_generation` and invalidates open result handles. Ordinary
//! batches bump `content_generation` only; open results stay readable.

use std::any::Any;
use std::sync::Arc;

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use crate::wtf8;

pub type EntryId = u32;
pub const NO_PARENT: EntryId = u32::MAX;

pub mod flags {
    pub const IS_DIR: u8 = 1;
    pub const TOMBSTONE: u8 = 2;
    pub const REPARSE: u8 = 4;
}

/// Mask an NTFS file reference number down to the record number (low 48 bits).
#[inline]
pub fn masked(record_or_frn: u64) -> u64 {
    record_or_frn & 0x0000_FFFF_FFFF_FFFF
}

/// One record produced by an initial-scan source (raw $MFT today, ReFS
/// enumeration in the future).
pub struct RawEntry<'a> {
    pub record: u64,
    pub parent_record: u64,
    /// Full FRN including the sequence value.
    pub frn: u64,
    pub name_utf16: &'a [u16],
    pub is_dir: bool,
    pub is_reparse: bool,
    pub size: u64,
    /// FILETIME (100ns ticks since 1601, UTC).
    pub mtime: i64,
}

pub struct VolumeIndex {
    name_pool: Vec<u8>,
    lower_pool: Vec<u8>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    parent: Vec<EntryId>,
    size: Vec<u64>,
    mtime: Vec<i64>,
    frn: Vec<u64>,
    flag: Vec<u8>,
    frn_map: FxHashMap<u64, EntryId>,
    perm_name: Vec<EntryId>,
    perm_size: Vec<EntryId>,
    perm_mtime: Vec<EntryId>,
    content_generation: u64,
    structural_generation: u64,
    tombstones: u32,
    /// Query-independent caches derived from index content (currently the
    /// dir-path memo), keyed by `content_generation`. Type-erased so the
    /// index stays ignorant of query-module types.
    derived_cache: Mutex<Option<(u64, Arc<dyn Any + Send + Sync>)>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Mtime,
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

    pub fn content_generation(&self) -> u64 {
        self.content_generation
    }

    pub fn structural_generation(&self) -> u64 {
        self.structural_generation
    }

    /// Return the cached content-derived value, rebuilding it with `build`
    /// when the content generation moved (or the type changed).
    pub(crate) fn cached_path_memo<T, F>(&self, build: F) -> Arc<T>
    where
        T: Any + Send + Sync,
        F: FnOnce() -> T,
    {
        let mut guard = self.derived_cache.lock();
        if let Some((generation, memo)) = guard.as_ref()
            && *generation == self.content_generation
            && let Ok(t) = memo.clone().downcast::<T>()
        {
            return t;
        }
        let t = Arc::new(build());
        *guard = Some((self.content_generation, t.clone()));
        t
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

    // ── Incremental mutation (USN batches; see module docs) ──────────────

    /// Insert or replace an entry for `record`. Replacement (same record
    /// number) tombstones the old entry — this is how renames work.
    /// Returns the new id. Caller must finish the batch with
    /// [`Self::merge_new_into_permutations`].
    pub fn upsert(&mut self, e: &RawEntry) -> EntryId {
        if let Some(old) = self.frn_map.get(&masked(e.record)).copied() {
            self.flag[old as usize] |= flags::TOMBSTONE;
            self.tombstones += 1;
        }
        let id = self.push_raw(e);
        self.frn_map.insert(masked(e.record), id);
        id
    }

    pub fn delete(&mut self, record: u64) -> Option<EntryId> {
        let id = self.frn_map.remove(&masked(record))?;
        self.flag[id as usize] |= flags::TOMBSTONE;
        self.tombstones += 1;
        Some(id)
    }

    /// Move `record` under a new parent. Cheap: no permutation depends on
    /// the path, and child paths rebuild lazily.
    pub fn reparent(&mut self, record: u64, new_parent_record: u64) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let parent = self
            .entry_by_record(new_parent_record)
            .unwrap_or(Self::ROOT);
        self.parent[id as usize] = parent;
        Some(id)
    }

    /// Update size/mtime in place (USN_REASON_DATA_* without a name change).
    pub fn update_stat(&mut self, record: u64, size: u64, mtime: i64) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        self.size[id as usize] = size;
        self.mtime[id as usize] = mtime;
        Some(id)
    }

    /// Merge entries `first_new..len` (already appended, unsorted) into all
    /// permutation arrays in one pass per key, then bump the content
    /// generation. Call once per USN batch.
    pub fn merge_new_into_permutations(&mut self, first_new: EntryId) {
        let new_ids: Vec<EntryId> = (first_new..self.len() as u32).collect();
        if !new_ids.is_empty() {
            for key in [SortKey::Name, SortKey::Size, SortKey::Mtime] {
                let mut batch = new_ids.clone();
                batch.sort_unstable_by(|&a, &b| self.cmp_by(key, a, b));
                let merged = {
                    let old = self.permutation(key);
                    let mut merged = Vec::with_capacity(old.len() + batch.len());
                    let (mut i, mut j) = (0, 0);
                    while i < old.len() && j < batch.len() {
                        if self.cmp_by(key, old[i], batch[j]).is_le() {
                            merged.push(old[i]);
                            i += 1;
                        } else {
                            merged.push(batch[j]);
                            j += 1;
                        }
                    }
                    merged.extend_from_slice(&old[i..]);
                    merged.extend_from_slice(&batch[j..]);
                    merged
                };
                match key {
                    SortKey::Name => self.perm_name = merged,
                    SortKey::Size => self.perm_size = merged,
                    SortKey::Mtime => self.perm_mtime = merged,
                }
            }
        }
        self.content_generation += 1;
    }

    pub fn tombstone_ratio(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.tombstones as f64 / self.len() as f64
        }
    }

    #[inline]
    fn cmp_by(&self, key: SortKey, a: EntryId, b: EntryId) -> std::cmp::Ordering {
        match key {
            SortKey::Name => self.lower_name(a).cmp(self.lower_name(b)).then(a.cmp(&b)),
            SortKey::Size => self.size(a).cmp(&self.size(b)).then(a.cmp(&b)),
            SortKey::Mtime => self.mtime(a).cmp(&self.mtime(b)).then(a.cmp(&b)),
        }
    }

    fn push_raw(&mut self, e: &RawEntry) -> EntryId {
        assert!(
            self.len() < u32::MAX as usize - 1,
            "volume entry count overflow"
        );
        let id = self.len() as EntryId;
        let off = self.name_pool.len();
        assert!(
            off + e.name_utf16.len() * 4 < u32::MAX as usize,
            "name pool overflow"
        );
        wtf8::push_wtf8_pair(e.name_utf16, &mut self.name_pool, &mut self.lower_pool);
        self.name_off.push(off as u32);
        self.name_len.push((self.name_pool.len() - off) as u16);
        // Parent is resolved against the map; unknown parents attach to root
        // (orphan records do occur in real MFTs).
        let parent = self
            .frn_map
            .get(&masked(e.parent_record))
            .copied()
            .unwrap_or(Self::ROOT);
        self.parent.push(parent);
        self.size.push(e.size);
        self.mtime.push(e.mtime);
        self.frn.push(e.frn);
        let mut f = 0u8;
        if e.is_dir {
            f |= flags::IS_DIR;
        }
        if e.is_reparse {
            f |= flags::REPARSE;
        }
        self.flag.push(f);
        id
    }
}

/// Two-pass builder for the initial scan: collect everything, then resolve
/// parents and sort the permutations (scan order ≠ parent-before-child).
pub struct VolumeIndexBuilder {
    idx: VolumeIndex,
    parent_records: Vec<u64>,
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

    pub fn finish(mut self) -> VolumeIndex {
        use rayon::prelude::*;

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
        self.idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw<'a>(
        record: u64,
        parent: u64,
        name: &'a [u16],
        is_dir: bool,
        size: u64,
        mtime: i64,
    ) -> RawEntry<'a> {
        RawEntry {
            record,
            parent_record: parent,
            frn: (1u64 << 48) | record,
            name_utf16: name,
            is_dir,
            is_reparse: false,
            size,
            mtime,
        }
    }

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// C:\ ├─ docs\ ├─ note.txt   docs comes *after* its child in scan order.
    fn build_sample() -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let note = u16s("Note.TXT");
        let docs = u16s("docs");
        let big = u16s("big.bin");
        b.push(raw(100, 50, &note, false, 10, 300)); // parent not yet pushed
        b.push(raw(50, 5, &docs, true, 0, 100));
        b.push(raw(60, 5, &big, false, 99_999, 200));
        b.finish()
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
    fn orphan_attaches_to_root() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let name = u16s("lost.txt");
        b.push(raw(7, 999_999, &name, false, 1, 1));
        let idx = b.finish();
        let id = idx.entry_by_record(7).unwrap();
        assert_eq!(idx.parent(id), VolumeIndex::ROOT);
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

    #[test]
    fn rename_is_tombstone_plus_new_entry() {
        let mut idx = build_sample();
        let old = idx.entry_by_record(100).unwrap();
        let first_new = idx.len() as u32;
        let renamed = u16s("renamed.txt");
        let mut e = raw(100, 50, &renamed, false, 10, 300);
        e.frn = idx.frn(old); // same FRN, new name
        let new_id = idx.upsert(&e);
        idx.merge_new_into_permutations(first_new);

        assert!(!idx.is_live(old));
        assert!(idx.is_live(new_id));
        assert_eq!(idx.entry_by_record(100), Some(new_id));
        assert_eq!(idx.name(new_id), b"renamed.txt");
        // Permutations contain the new id in sorted position.
        let pos = idx
            .permutation(SortKey::Name)
            .iter()
            .position(|&i| i == new_id)
            .unwrap();
        let perm = idx.permutation(SortKey::Name);
        if pos > 0 {
            assert!(idx.lower_name(perm[pos - 1]) <= idx.lower_name(new_id));
        }
        if pos + 1 < perm.len() {
            assert!(idx.lower_name(new_id) <= idx.lower_name(perm[pos + 1]));
        }
    }

    #[test]
    fn delete_and_reparent() {
        let mut idx = build_sample();
        let big = idx.entry_by_record(60).unwrap();
        idx.reparent(60, 50);
        let docs = idx.entry_by_record(50).unwrap();
        assert_eq!(idx.parent(big), docs);

        idx.delete(60);
        assert!(!idx.is_live(big));
        assert_eq!(idx.entry_by_record(60), None);
        assert!(idx.tombstone_ratio() > 0.0);
    }
}
