//! In-memory per-volume index: struct-of-arrays, two string pools with shared
//! offsets, FRN map, and pre-sorted permutations for instant sorting
//! (docs/ARCHITECTURE.md).
//!
//! Mutation model (keeps the permutation arrays merge-only):
//! - create  → append entry + merge into permutations
//! - delete  → tombstone flag only
//! - rename  → files: tombstone old + append new entry (same FRN);
//!   dirs: in-place (children point at the EntryId), repositioned in perm_name
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
    /// Raw FILE_ATTRIBUTE_HIDDEN.
    pub const HIDDEN: u8 = 8;
    /// Raw FILE_ATTRIBUTE_SYSTEM.
    pub const SYSTEM: u8 = 16;
    /// Computed: this entry or any ancestor carries HIDDEN|SYSTEM. Queries
    /// skip these by default (toggleable). Kept in sync on insert/move; a
    /// subtree moved out of an excluded branch keeps stale bits until the
    /// next full rescan (same accepted-limitation class as dir renames).
    pub const EXCLUDED: u8 = 32;
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
    pub is_hidden: bool,
    pub is_system: bool,
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
        self.recompute_excluded(id);
        Some(id)
    }

    /// Rename/move a *directory* in place. Directories must keep their
    /// EntryId stable — children's `parent` fields point at it — so instead
    /// of tombstone+new (the file path), the name is swapped and the entry is
    /// repositioned inside `perm_name`. O(len) per rename; directory renames
    /// are rare enough that this beats invalidating every child.
    pub fn rename_dir_in_place(
        &mut self,
        record: u64,
        name_utf16: &[u16],
        new_parent_record: u64,
    ) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let pos = self.perm_name.iter().position(|&x| x == id)?;
        self.perm_name.remove(pos);

        let off = self.name_pool.len();
        wtf8::push_wtf8_pair(name_utf16, &mut self.name_pool, &mut self.lower_pool);
        self.name_off[id as usize] = off as u32;
        self.name_len[id as usize] = (self.name_pool.len() - off) as u16;
        let parent = self
            .entry_by_record(new_parent_record)
            .unwrap_or(Self::ROOT);
        if parent != id {
            self.parent[id as usize] = parent;
        }
        self.recompute_excluded(id);

        let ins = self
            .perm_name
            .binary_search_by(|&x| self.cmp_by(SortKey::Name, x, id))
            .unwrap_or_else(|e| e);
        self.perm_name.insert(ins, id);
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
        if e.is_hidden {
            f |= flags::HIDDEN;
        }
        if e.is_system {
            f |= flags::SYSTEM;
        }
        // Provisional during the initial scan (parents may resolve later —
        // the builder recomputes in finish()); exact on the USN path where
        // parents are already live.
        let parent_excluded = self
            .flag
            .get(parent as usize)
            .is_some_and(|pf| pf & flags::EXCLUDED != 0);
        if e.is_hidden || e.is_system || parent_excluded {
            f |= flags::EXCLUDED;
        }
        self.flag.push(f);
        id
    }

    /// Re-derive EXCLUDED for `id` from its own H/S bits and current parent.
    fn recompute_excluded(&mut self, id: EntryId) {
        let p = self.parent[id as usize];
        let inherited = p != NO_PARENT && p != id && self.flag[p as usize] & flags::EXCLUDED != 0;
        let own = self.flag[id as usize] & (flags::HIDDEN | flags::SYSTEM) != 0;
        if own || inherited {
            self.flag[id as usize] |= flags::EXCLUDED;
        } else {
            self.flag[id as usize] &= !flags::EXCLUDED;
        }
    }

    /// Update raw attribute bits (USN BASIC_INFO_CHANGE) and the derived
    /// EXCLUDED bit.
    pub fn update_attrs(
        &mut self,
        record: u64,
        is_hidden: bool,
        is_system: bool,
    ) -> Option<EntryId> {
        let id = self.entry_by_record(record)?;
        let f = &mut self.flag[id as usize];
        *f = (*f & !(flags::HIDDEN | flags::SYSTEM))
            | if is_hidden { flags::HIDDEN } else { 0 }
            | if is_system { flags::SYSTEM } else { 0 };
        self.recompute_excluded(id);
        Some(id)
    }
}

// ── Snapshot persistence (.fmfidx) ──────────────────────────────────────
//
// Header (magic, version, journal checkpoint) + raw little-endian column
// dumps + trailing xxhash64. Machine-local cache only — corruption or any
// mismatch falls back to a full rescan, so the format favors speed over
// portability (docs/ARCHITECTURE.md).

// 02: flag byte gained HIDDEN/SYSTEM/EXCLUDED bits — older snapshots must
// trigger a full rescan rather than load with wrong semantics.
const SNAPSHOT_MAGIC: &[u8; 8] = b"FMFIDX02";

fn pod_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // Safety: T is a plain-old-data column type (u8/u16/u32/u64/i64).
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

fn write_vec<T: Copy, W: std::io::Write>(
    w: &mut W,
    h: &mut xxhash_rust::xxh64::Xxh64,
    v: &[T],
) -> std::io::Result<()> {
    let bytes = pod_bytes(v);
    let len = (bytes.len() as u64).to_le_bytes();
    h.update(&len);
    w.write_all(&len)?;
    h.update(bytes);
    w.write_all(bytes)
}

fn read_vec<T: Copy + Default, R: std::io::Read>(
    r: &mut R,
    h: &mut xxhash_rust::xxh64::Xxh64,
) -> std::io::Result<Vec<T>> {
    use std::io::{Error, ErrorKind};
    let mut len8 = [0u8; 8];
    r.read_exact(&mut len8)?;
    h.update(&len8);
    let len = u64::from_le_bytes(len8) as usize;
    let elem = std::mem::size_of::<T>();
    if !len.is_multiple_of(elem) {
        return Err(Error::new(ErrorKind::InvalidData, "section size mismatch"));
    }
    let mut out = vec![T::default(); len / elem];
    // Safety: same POD reasoning as pod_bytes, writable side.
    let bytes = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<u8>(), len) };
    r.read_exact(bytes)?;
    h.update(bytes);
    Ok(out)
}

impl VolumeIndex {
    /// Serialize the index plus the USN checkpoint (`journal_id`, `next_usn`).
    pub fn write_snapshot<W: std::io::Write>(
        &self,
        w: &mut W,
        journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        let mut h = xxhash_rust::xxh64::Xxh64::new(0);
        let mut head = Vec::with_capacity(32);
        head.extend_from_slice(SNAPSHOT_MAGIC);
        head.extend_from_slice(&journal_id.to_le_bytes());
        head.extend_from_slice(&next_usn.to_le_bytes());
        head.extend_from_slice(&(self.len() as u64).to_le_bytes());
        h.update(&head);
        w.write_all(&head)?;

        write_vec(w, &mut h, &self.name_pool)?;
        write_vec(w, &mut h, &self.lower_pool)?;
        write_vec(w, &mut h, &self.name_off)?;
        write_vec(w, &mut h, &self.name_len)?;
        write_vec(w, &mut h, &self.parent)?;
        write_vec(w, &mut h, &self.size)?;
        write_vec(w, &mut h, &self.mtime)?;
        write_vec(w, &mut h, &self.frn)?;
        write_vec(w, &mut h, &self.flag)?;
        write_vec(w, &mut h, &self.perm_name)?;
        write_vec(w, &mut h, &self.perm_size)?;
        write_vec(w, &mut h, &self.perm_mtime)?;
        w.write_all(&h.digest().to_le_bytes())
    }

    /// Load a snapshot; returns the index and the persisted (journal_id,
    /// next_usn) checkpoint. Any structural or checksum mismatch is an error
    /// — callers fall back to a full rescan.
    pub fn read_snapshot<R: std::io::Read>(r: &mut R) -> std::io::Result<(Self, u64, i64)> {
        use std::io::{Error, ErrorKind};
        let bad = |m: &str| Error::new(ErrorKind::InvalidData, m.to_string());

        let mut h = xxhash_rust::xxh64::Xxh64::new(0);
        let mut head = [0u8; 32];
        r.read_exact(&mut head)?;
        if &head[..8] != SNAPSHOT_MAGIC {
            return Err(bad("bad magic"));
        }
        h.update(&head);
        let journal_id = u64::from_le_bytes(head[8..16].try_into().unwrap());
        let next_usn = i64::from_le_bytes(head[16..24].try_into().unwrap());
        let count = u64::from_le_bytes(head[24..32].try_into().unwrap()) as usize;

        let name_pool: Vec<u8> = read_vec(r, &mut h)?;
        let lower_pool: Vec<u8> = read_vec(r, &mut h)?;
        let name_off: Vec<u32> = read_vec(r, &mut h)?;
        let name_len: Vec<u16> = read_vec(r, &mut h)?;
        let parent: Vec<u32> = read_vec(r, &mut h)?;
        let size: Vec<u64> = read_vec(r, &mut h)?;
        let mtime: Vec<i64> = read_vec(r, &mut h)?;
        let frn: Vec<u64> = read_vec(r, &mut h)?;
        let flag: Vec<u8> = read_vec(r, &mut h)?;
        let perm_name: Vec<u32> = read_vec(r, &mut h)?;
        let perm_size: Vec<u32> = read_vec(r, &mut h)?;
        let perm_mtime: Vec<u32> = read_vec(r, &mut h)?;

        let mut digest = [0u8; 8];
        r.read_exact(&mut digest)?;
        if u64::from_le_bytes(digest) != h.digest() {
            return Err(bad("checksum mismatch"));
        }
        let columns_ok = [
            name_off.len(),
            name_len.len(),
            parent.len(),
            size.len(),
            mtime.len(),
            frn.len(),
            flag.len(),
            perm_name.len(),
            perm_size.len(),
            perm_mtime.len(),
        ]
        .iter()
        .all(|&l| l == count);
        if !columns_ok || name_pool.len() != lower_pool.len() {
            return Err(bad("column length mismatch"));
        }

        let mut frn_map = FxHashMap::default();
        let mut tombstones = 0u32;
        for (i, &f) in flag.iter().enumerate() {
            if f & flags::TOMBSTONE != 0 {
                tombstones += 1;
            } else {
                frn_map.insert(masked(frn[i]), i as EntryId);
            }
        }

        Ok((
            Self {
                name_pool,
                lower_pool,
                name_off,
                name_len,
                parent,
                size,
                mtime,
                frn,
                flag,
                frn_map,
                perm_name,
                perm_size,
                perm_mtime,
                content_generation: 0,
                structural_generation: 0,
                tombstones,
                derived_cache: Mutex::new(None),
            },
            journal_id,
            next_usn,
        ))
    }

    /// Atomic save: write to `<path>.tmp`, then rename over the target.
    pub fn save_to(
        &self,
        path: &std::path::Path,
        journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        let tmp = path.with_extension("fmfidx.tmp");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        {
            let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            self.write_snapshot(&mut w, journal_id, next_usn)?;
            use std::io::Write;
            w.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    pub fn load_from(path: &std::path::Path) -> std::io::Result<(Self, u64, i64)> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        Self::read_snapshot(&mut r)
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
            is_hidden: false,
            is_system: false,
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

    #[test]
    fn snapshot_roundtrip_preserves_everything() {
        let mut idx = build_sample();
        idx.delete(60); // include a tombstone
        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 0xDEAD_BEEF_u64, 12345)
            .unwrap();
        let (loaded, journal_id, next_usn) =
            VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();

        assert_eq!(journal_id, 0xDEAD_BEEF_u64);
        assert_eq!(next_usn, 12345);
        assert_eq!(loaded.len(), idx.len());
        assert_eq!(loaded.live_len(), idx.live_len());
        // Deleted record stays deleted; live lookups and paths survive.
        assert_eq!(loaded.entry_by_record(60), None);
        let note = loaded.entry_by_record(100).unwrap();
        let mut p = Vec::new();
        loaded.append_path(note, &mut p);
        assert_eq!(p, b"C:\\docs\\Note.TXT");
        assert_eq!(
            loaded.permutation(SortKey::Name),
            idx.permutation(SortKey::Name)
        );
    }

    #[test]
    fn snapshot_corruption_is_detected() {
        let idx = build_sample();
        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 1).unwrap();
        let mid = buf.len() / 2;
        buf[mid] ^= 0xFF;
        assert!(VolumeIndex::read_snapshot(&mut buf.as_slice()).is_err());

        let mut truncated = Vec::new();
        idx.write_snapshot(&mut truncated, 1, 1).unwrap();
        truncated.truncate(truncated.len() - 3);
        assert!(VolumeIndex::read_snapshot(&mut truncated.as_slice()).is_err());
    }

    fn raw_attr<'a>(
        record: u64,
        parent: u64,
        name: &'a [u16],
        is_dir: bool,
        is_hidden: bool,
        is_system: bool,
    ) -> RawEntry<'a> {
        RawEntry {
            record,
            parent_record: parent,
            frn: (1u64 << 48) | record,
            name_utf16: name,
            is_dir,
            is_reparse: false,
            is_hidden,
            is_system,
            size: 0,
            mtime: 0,
        }
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

    #[test]
    fn usn_insert_and_moves_track_exclusion() {
        let sysdir = u16s("sysdir");
        let normal = u16s("docs");
        let mut b = VolumeIndexBuilder::new("C:", 5);
        b.push(raw_attr(10, 5, &sysdir, true, false, true));
        b.push(raw_attr(20, 5, &normal, true, false, false));
        let mut idx = b.finish();

        // New plain file created under the system dir → inherits.
        let name = u16s("payload.tmp");
        let first_new = idx.len() as u32;
        let id = idx.upsert(&raw_attr(30, 10, &name, false, false, false));
        idx.merge_new_into_permutations(first_new);
        assert!(idx.is_excluded(id));

        // Moved out into a normal dir → bit clears.
        idx.reparent(30, 20);
        assert!(!idx.is_excluded(id));

        // Attribute change marks it hidden → re-excluded.
        idx.update_attrs(30, true, false);
        assert!(idx.is_excluded(id));
    }
}
