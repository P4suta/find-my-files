use std::any::Any;
use std::sync::Arc;

use parking_lot::Mutex;
use rustc_hash::{FxBuildHasher, FxHashMap};

use super::frn::FrnIndex;
use super::{EntryId, Frn, NO_PARENT, RecordNo, SortKey, flags};

/// In-memory per-volume index.
///
/// Struct-of-arrays entry columns, two string pools sharing one offset/length
/// table, an FRN map, and the always-sorted name permutation
/// (docs/ARCHITECTURE.md). One instance per indexed volume.
pub struct VolumeIndex {
    /// The contiguous, sweepable **dictionary** of *distinct* folded names
    /// (ADR-0032). Each entry indexes it through `name_id`; a name's bytes are
    /// `dict_pool[dict_off[name_id]..dict_off[name_id+1]]` — the dict is
    /// gapless and `dict_off` ascending, so a name's length is the gap to the
    /// next offset (`dict_pool.len()` for the last; ADR-0033 dropped the
    /// separate `dict_len` column). `name_id` is assigned in dict-append
    /// order, so the sweep maps a hit to a `name_id` with a monotonic cursor
    /// and needs no offset table. Most names fold to themselves (ADR-0004);
    /// the original spelling is stored per-entry only where it differs — in
    /// `orig_pool` at `orig_off`, same length as the fold (length-preserving,
    /// wtf8.rs). `orig_off == u32::MAX` means the folded bytes *are* original.
    pub(super) dict_pool: Vec<u8>,
    pub(super) dict_off: Vec<u32>,
    pub(super) name_id: Vec<u32>,
    pub(super) orig_pool: Vec<u8>,
    pub(super) orig_off: Vec<u32>,
    pub(super) parent: Vec<EntryId>,
    /// File sizes < `u32::MAX`, 4 bytes per entry; `u32::MAX` is the sentinel
    /// for the overflow map (≥4GiB files, ADR-0007). Read through
    /// [`VolumeIndex::size`].
    pub(super) size_lo: Vec<u32>,
    pub(super) size_ovf: FxHashMap<EntryId, u64>,
    /// Last-modification time as Unix-epoch **seconds** in a `u32` (ADR-0031,
    /// −4 B/entry vs a raw FILETIME `i64`). `0` is the "unknown timestamp"
    /// sentinel. Encode/decode through
    /// `query::dates::mtime_{ticks_to_secs,secs_to_ticks}`; read an entry's
    /// value through [`VolumeIndex::mtime`].
    pub(super) mtime: Vec<u32>,
    pub(super) frn: Vec<u64>,
    pub(super) flag: Vec<u8>,
    pub(super) frn_index: FrnIndex,
    /// The one always-maintained permutation: name order is the default
    /// sort and the merge target of every USN batch. Size/mtime orders are
    /// lazily derived caches (`query::memo::{SizePerm`, `MtimePerm`}) — built on
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
    /// Reclaimable original-spelling bytes left by tombstoned rows and
    /// in-place dir renames (folded bytes are shared in the dictionary and
    /// their bloat is tracked by `dict_appends_since_dedup` instead,
    /// ADR-0032). Compaction-trigger input. Not persisted — recomputed from
    /// tombstones on restore, so rename gaps make it a lower bound there.
    pub(super) dead_name_bytes: u64,
    /// Dict entries appended since the last `dedup_dict` (USN creates append
    /// un-deduped, ADR-0032). A churn-trigger input so a pure-create burst
    /// compacts before the dictionary bloats. Reset by `dedup_dict`.
    pub(super) dict_appends_since_dedup: u32,
    /// Query-independent caches derived from index content (dir-path memo,
    /// …) keyed by `content_generation` and value type.
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

// ── Shared column accessors ──────────────────────────────────────────────
// `VolumeIndex` owns its columns; `SortColumns` borrows them so permutation
// maintenance can hold the `&mut` perm array alongside the keys. Both read an
// entry's folded name and size identically — these free functions are the one
// definition each delegates to, so the pair can never drift (the same hazard
// `SortColumns`'s own doc cites for `cmp_by`).

/// Folded name bytes of `id`, resolved through `name_id` into the dictionary.
/// The name ends where the next one begins (the dict is gapless), or at the
/// pool end for the last name (ADR-0033).
#[inline]
fn dict_lower_name<'a>(
    dict_pool: &'a [u8],
    dict_off: &[u32],
    name_id: &[u32],
    id: EntryId,
) -> &'a [u8] {
    let nid = name_id[id as usize] as usize;
    let off = dict_off[nid] as usize;
    let end = dict_off
        .get(nid + 1)
        .map_or(dict_pool.len(), |&e| e as usize);
    &dict_pool[off..end]
}

/// Size of `id` read through the u32 column + overflow map (ADR-0007).
#[inline]
fn column_size(size_lo: &[u32], size_ovf: &FxHashMap<EntryId, u64>, id: EntryId) -> u64 {
    match size_lo[id as usize] {
        u32::MAX => size_ovf[&id],
        v => v as u64,
    }
}

impl VolumeIndex {
    /// Total entry slots, live plus tombstoned (the column length).
    pub const fn len(&self) -> usize {
        self.name_id.len()
    }

    /// True when no entries have ever been appended.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Live entry count: total slots minus tombstones.
    pub const fn live_len(&self) -> usize {
        self.len() - self.tombstones as usize
    }

    /// The volume root's [`EntryId`] (always slot 0).
    pub const ROOT: EntryId = 0;

    /// The original-spelling name. Fold-identical entries (most of them)
    /// borrow straight from the folded pool — the bytes are the same.
    #[inline]
    pub fn name(&self, id: EntryId) -> &[u8] {
        match self.orig_off[id as usize] {
            u32::MAX => self.lower_name(id),
            off => {
                let len = self.name_len_of(id);
                &self.orig_pool[off as usize..off as usize + len]
            }
        }
    }

    /// The case-folded name bytes of `id` (ADR-0004), straight from the
    /// folded pool — the form every matcher compares against.
    #[inline]
    pub fn lower_name(&self, id: EntryId) -> &[u8] {
        dict_lower_name(&self.dict_pool, &self.dict_off, &self.name_id, id)
    }

    /// True while `id` is a real entry — false once it has been tombstoned.
    #[inline]
    pub fn is_live(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::TOMBSTONE == 0
    }

    /// Hidden/system (or under such a branch) — skipped by default queries.
    #[inline]
    pub fn is_excluded(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::EXCLUDED != 0
    }

    /// True when `id` is a directory rather than a file.
    #[inline]
    pub fn is_dir(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::IS_DIR != 0
    }

    /// True when `id` is a reparse point (symlink, junction, mount point).
    #[inline]
    pub fn is_reparse(&self, id: EntryId) -> bool {
        self.flag[id as usize] & flags::REPARSE != 0
    }

    /// File size of `id` in bytes, read through the u32 column and the
    /// overflow map for ≥4 GiB files (ADR-0007).
    #[inline]
    pub fn size(&self, id: EntryId) -> u64 {
        column_size(&self.size_lo, &self.size_ovf, id)
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

    /// Last-modification time of `id` as a Windows FILETIME tick count,
    /// reconstructed to the second from the stored `u32` Unix-seconds column
    /// (ADR-0031); the `0` "unknown timestamp" sentinel maps back to `0`.
    #[inline]
    pub fn mtime(&self, id: EntryId) -> i64 {
        crate::query::dates::mtime_secs_to_ticks(self.mtime[id as usize])
    }

    /// The [`EntryId`] of `id`'s parent directory ([`NO_PARENT`] at the root).
    #[inline]
    pub fn parent(&self, id: EntryId) -> EntryId {
        self.parent[id as usize]
    }

    /// The NTFS File Reference Number of `id`.
    #[inline]
    pub fn frn(&self, id: EntryId) -> Frn {
        Frn(self.frn[id as usize])
    }

    /// The live entry for a record number, if any. Pass a [`RecordNo`] (or a
    /// raw record-number `u64`); derive one from a full reference with
    /// [`Frn::record`] — the type stops a full FRN being mistaken for a key.
    pub fn entry_by_record(&self, record: impl Into<RecordNo>) -> Option<EntryId> {
        self.frn_index.lookup(record.into(), &self.frn, &self.flag)
    }

    // Raw dictionary access for the pool-scan query kernel (same crate only).
    #[inline]
    pub(crate) fn dict_pool_bytes(&self) -> &[u8] {
        &self.dict_pool
    }

    /// Per-`name_id` offsets into the dict pool — ascending by construction.
    #[inline]
    pub(crate) fn dict_offs(&self) -> &[u32] {
        &self.dict_off
    }

    /// An entry's dictionary id.
    #[inline]
    pub(crate) fn name_id_of(&self, id: EntryId) -> u32 {
        self.name_id[id as usize]
    }

    /// Folded-name length of `id`: the gap from its dict offset to the next
    /// (or the pool end for the last name; ADR-0033).
    #[inline]
    pub(crate) fn name_len_of(&self, id: EntryId) -> usize {
        let nid = self.name_id[id as usize] as usize;
        let off = self.dict_off[nid] as usize;
        let end = self
            .dict_off
            .get(nid + 1)
            .map_or(self.dict_pool.len(), |&e| e as usize);
        end - off
    }

    /// True when the entry's original spelling is its folded form — the
    /// case-exact matchers' fast path: such a name can never contain a
    /// needle with fold-unstable characters, and for fold-stable needles
    /// the folded comparison *is* the exact comparison.
    #[inline]
    pub(crate) fn is_fold_identical(&self, id: EntryId) -> bool {
        self.orig_off[id as usize] == u32::MAX
    }

    /// The content generation — bumped by every USN batch; open result
    /// handles stay readable across it (docs/ARCHITECTURE.md, generation 2-tier).
    pub const fn content_generation(&self) -> u64 {
        self.content_generation
    }

    /// The structural generation — bumped only by compaction/rebuild, which
    /// hard-stales open result handles (docs/ARCHITECTURE.md, generation 2-tier).
    pub const fn structural_generation(&self) -> u64 {
        self.structural_generation
    }

    pub(crate) const fn dir_topology_generation(&self) -> u64 {
        self.dir_topology_generation
    }

    /// Carry the structural generation across a rebuild: a freshly built
    /// index replacing one whose generation was `prev` must read as strictly
    /// newer, so open result handles go hard-stale (docs/ARCHITECTURE.md,
    /// generation 2-tier). Compaction (M2) will reuse this.
    pub(crate) const fn bump_structural_from(&mut self, prev: u64) {
        self.structural_generation = prev + 1;
    }

    /// Return the cached content-derived value of type `T`. On a generation
    /// change `build` receives the previous generation's value so it can
    /// extend it incrementally instead of rebuilding from scratch; all cached
    /// types are invalidated together on a generation change.
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
        let offsets = (self.name_id.capacity() * 4
            + self.dict_off.capacity() * 4
            + self.orig_off.capacity() * 4) as u64;
        // perm_name only — the lazy size/mtime permutations are accounted
        // with the derived caches (`derived_cache_bytes`).
        let perms = (self.perm_name.capacity() * 4) as u64;
        // Field name kept for FFI/JSON compatibility; the structure is the
        // sorted FRN permutation (index/frn.rs).
        let frn_map = self.frn_index.bytes();
        let mut s = crate::metrics::IndexStats {
            volume: volume.to_string(),
            entries: n,
            live_entries: self.live_len() as u64,
            tombstones: self.tombstones as u64,
            // Field name kept for FFI/JSON compatibility; this is the
            // original-spelling overflow pool (fold-identical names live
            // only in lower_pool).
            name_pool_bytes: self.orig_pool.capacity() as u64,
            lower_pool_bytes: self.dict_pool.capacity() as u64,
            offsets_bytes: offsets,
            parent_bytes: (self.parent.capacity() * 4) as u64,
            // Column + the overflow map (hashbrown estimate: (K,V) slot +
            // 1 control byte per capacity slot; the map is tiny, ADR-0007).
            size_bytes: (self.size_lo.capacity() * 4
                + self.size_ovf.capacity() * (std::mem::size_of::<(EntryId, u64)>() + 1))
                as u64,
            mtime_bytes: (self.mtime.capacity() * 4) as u64,
            frn_bytes: (self.frn.capacity() * 8) as u64,
            flag_bytes: self.flag.capacity() as u64,
            permutations_bytes: perms,
            frn_map_bytes: frn_map,
            dead_name_bytes: self.dead_name_bytes,
            content_generation: self.content_generation,
            structural_generation: self.structural_generation,
            ..Default::default()
        };
        // dead_name_bytes already counts every abandoned copy across both
        // pools (folded always, original when present).
        let pool_bytes = s.name_pool_bytes + s.lower_pool_bytes;
        s.pool_garbage_ratio = if pool_bytes > 0 {
            self.dead_name_bytes as f64 / pool_bytes as f64
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
        self.dict_pool.shrink_to_fit();
        self.dict_off.shrink_to_fit();
        self.name_id.shrink_to_fit();
        self.orig_pool.shrink_to_fit();
        self.orig_off.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.size_lo.shrink_to_fit();
        self.size_ovf.shrink_to_fit();
        self.mtime.shrink_to_fit();
        self.frn.shrink_to_fit();
        self.flag.shrink_to_fit();
        self.perm_name.shrink_to_fit();
    }

    /// Rebuild the dictionary over the *distinct* folded names of the live
    /// entries (ADR-0032): collapse the un-deduped appends left by the build
    /// and by USN creates, remap every `name_id`, and reset the churn
    /// counter. A transient `FxHashMap` interner keyed by the old dict bytes;
    /// tombstoned entries' names drop out. The original-spelling pool is
    /// deduped in the same place by its sibling [`Self::dedup_orig`]. O(n) over
    /// a read-only old dict plus one freshly built pool.
    pub(super) fn dedup_dict(&mut self) {
        let n = self.len();
        let mut new_pool: Vec<u8> = Vec::with_capacity(self.dict_pool.len());
        let mut new_off: Vec<u32> = Vec::new();
        let mut new_name_id: Vec<u32> = vec![0u32; n];
        {
            let (dict_pool, dict_off, name_id, flag) =
                (&self.dict_pool, &self.dict_off, &self.name_id, &self.flag);
            // Pre-size for ~half-distinct names (ADR-0033 Lever 6): skips the
            // rehash growth across n inserts.
            let mut interner: FxHashMap<&[u8], u32> =
                FxHashMap::with_capacity_and_hasher(n / 2, FxBuildHasher);
            for id in 0..n as u32 {
                if flag[id as usize] & flags::TOMBSTONE != 0 {
                    continue;
                }
                let nid = name_id[id as usize] as usize;
                let off = dict_off[nid] as usize;
                let end = dict_off
                    .get(nid + 1)
                    .map_or(dict_pool.len(), |&e| e as usize);
                let folded = &dict_pool[off..end];
                let new_id = *interner.entry(folded).or_insert_with(|| {
                    let assigned = new_off.len() as u32;
                    new_off.push(new_pool.len() as u32);
                    new_pool.extend_from_slice(folded);
                    assigned
                });
                new_name_id[id as usize] = new_id;
            }
        }
        self.dict_pool = new_pool;
        self.dict_off = new_off;
        self.name_id = new_name_id;
        self.dict_appends_since_dedup = 0;
    }

    /// Rebuild the original-spelling pool over the *distinct* live originals
    /// (ADR-0033 Lever 1). Most names fold to themselves and own no original
    /// copy (`orig_off == u32::MAX`); the rest store their original verbatim,
    /// and those originals duplicate heavily across the volume (every `README`,
    /// `LICENSE`, `Makefile`). A transient interner keyed by the original bytes
    /// collapses them, pointing each `orig_off` at the one shared copy — on
    /// real C: 562k differing entries fold to 221k distinct originals (≈−4.5
    /// B/entry). No length table is needed: the fold is length-preserving
    /// (ADR-0004), so an original's length is its entry's folded length
    /// (`name_len_of`). Runs beside [`Self::dedup_dict`] at `finish`/`compacted`.
    /// O(n) over a read-only old pool plus one freshly built pool.
    pub(super) fn dedup_orig(&mut self) {
        let n = self.len();
        let mut new_pool: Vec<u8> = Vec::with_capacity(self.orig_pool.len());
        // u32::MAX (fold-identical) is the default; only differing entries are
        // repointed below.
        let mut new_off: Vec<u32> = vec![u32::MAX; n];
        {
            let (orig_pool, orig_off, name_id, dict_off, dict_pool, flag) = (
                &self.orig_pool,
                &self.orig_off,
                &self.name_id,
                &self.dict_off,
                &self.dict_pool,
                &self.flag,
            );
            let mut interner: FxHashMap<&[u8], u32> =
                FxHashMap::with_capacity_and_hasher(n / 2, FxBuildHasher);
            for id in 0..n as u32 {
                if flag[id as usize] & flags::TOMBSTONE != 0 {
                    continue;
                }
                let off = orig_off[id as usize];
                if off == u32::MAX {
                    continue; // fold-identical: no original copy to dedup
                }
                // The original's length is the entry's folded length (the fold
                // is length-preserving, ADR-0004), read through the dictionary.
                let nid = name_id[id as usize] as usize;
                let dlen = dict_off
                    .get(nid + 1)
                    .map_or(dict_pool.len(), |&e| e as usize)
                    - dict_off[nid] as usize;
                let orig = &orig_pool[off as usize..off as usize + dlen];
                // The interner value is the byte offset of the one shared copy;
                // a real offset is always < u32::MAX (the pool overflow guard in
                // `push_orig_if_differs`), so it never collides with the
                // fold-identical sentinel.
                let new_o = *interner.entry(orig).or_insert_with(|| {
                    let assigned = new_pool.len() as u32;
                    new_pool.extend_from_slice(orig);
                    assigned
                });
                new_off[id as usize] = new_o;
            }
        }
        self.orig_pool = new_pool;
        self.orig_off = new_off;
    }

    /// The always-maintained name-sorted permutation: entry ids in default
    /// (folded-name) sort order, the merge target of every USN batch.
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
            // The synthetic scope-mode ROOT (ADR-0024) carries no name; skip
            // it (name + separator) so multi-root paths don't gain a leading
            // `\`. Real $MFT entries always have a name, so this is inert for
            // the privileged path (its ROOT is the volume label, e.g. "C:").
            let name = self.name(c);
            if !name.is_empty() {
                out.extend_from_slice(name);
                out.push(b'\\');
            }
        }
    }

    /// Fraction of slots that are tombstones (0.0–1.0) — the compaction
    /// trigger input. 0.0 for an empty index.
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
            dict_pool: &self.dict_pool,
            dict_off: &self.dict_off,
            name_id: &self.name_id,
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
    dict_pool: &'a [u8],
    dict_off: &'a [u32],
    name_id: &'a [u32],
    size_lo: &'a [u32],
    size_ovf: &'a FxHashMap<EntryId, u64>,
    mtime: &'a [u32],
}

impl<'a> SortColumns<'a> {
    pub(super) const fn new(
        dict_pool: &'a [u8],
        dict_off: &'a [u32],
        name_id: &'a [u32],
        size_lo: &'a [u32],
        size_ovf: &'a FxHashMap<EntryId, u64>,
        mtime: &'a [u32],
    ) -> Self {
        Self {
            dict_pool,
            dict_off,
            name_id,
            size_lo,
            size_ovf,
            mtime,
        }
    }

    #[inline]
    fn lower_name(&self, id: EntryId) -> &[u8] {
        dict_lower_name(self.dict_pool, self.dict_off, self.name_id, id)
    }

    #[inline]
    fn size_of(&self, id: EntryId) -> u64 {
        column_size(self.size_lo, self.size_ovf, id)
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
