// ── Snapshot persistence (.fmfidx) ──────────────────────────────────────
//
// Header (magic, version, journal checkpoint) + raw little-endian column
// dumps + trailing xxhash64. Machine-local cache only — corruption or any
// mismatch falls back to a full rescan, so the format favors speed over
// portability (docs/ARCHITECTURE.md).

use parking_lot::Mutex;

use super::{EntryId, VolumeIndex, flags};

// Any semantic change to a section bumps the version in the magic: older
// snapshots must fail the magic check and trigger a full rescan rather
// than load with wrong semantics (ADR-0010, no backward compatibility).
const SNAPSHOT_MAGIC: &[u8; 8] = b"FMFIDX07";

const fn pod_bytes<T: Copy>(v: &[T]) -> &[u8] {
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

fn read_vec<T: Copy, R: std::io::Read>(
    r: &mut R,
    h: &mut xxhash_rust::xxh64::Xxh64,
) -> std::io::Result<Vec<T>> {
    use std::io::{Error, ErrorKind};
    // A corrupt length must come back as Err (the caller full-rescans), never be
    // handed to the allocator. `try_reserve_exact` below already turns a failed
    // reservation into a clean Err in production, but the request still goes to
    // the allocator first — and under a fuzzer that is a process abort, not an
    // Err: a multi-exabyte claim trips AddressSanitizer's allocation ceiling, and
    // even a few-GB claim trips libFuzzer's OOM guard (both surfaced by the
    // index_snapshot target). Reject anything past a ceiling no real section
    // approaches — the whole index at the 1M-file target is ~100 MB, so a single
    // section past 1 GiB (10× the entire index) is corruption — which also keeps
    // the bounded allocation under the fuzzer's limit, so "corrupt → Err, not
    // abort" holds while fuzzing the structural decode below.
    const MAX_SECTION_BYTES: usize = 1 << 30; // 1 GiB
    let mut len8 = [0u8; 8];
    r.read_exact(&mut len8)?;
    h.update(&len8);
    let len = u64::from_le_bytes(len8) as usize;
    if len > MAX_SECTION_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "section length implausibly large",
        ));
    }
    let elem = std::mem::size_of::<T>();
    if !len.is_multiple_of(elem) {
        return Err(Error::new(ErrorKind::InvalidData, "section size mismatch"));
    }
    // The length prefix is untrusted input: a corrupt value must come back
    // as Err (→ full-rescan fallback), not abort the process. try_reserve
    // turns an absurd claim into a clean Err; a plausible-but-lying length
    // then fails at read_exact EOF and the buffer drops.
    let elems = len / elem;
    let mut out: Vec<T> = Vec::new();
    if out.try_reserve_exact(elems).is_err() {
        return Err(Error::new(ErrorKind::InvalidData, "section size overflow"));
    }
    // Safety: same POD reasoning as pod_bytes, writable side — the spare
    // capacity is fully overwritten before set_len, and read failures leave
    // the length at 0.
    let bytes = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<u8>(), len) };
    for chunk in bytes.chunks_mut(8 << 20) {
        r.read_exact(chunk)?;
        h.update(chunk);
    }
    unsafe { out.set_len(elems) };
    Ok(out)
}

impl VolumeIndex {
    /// Serialize the index plus the USN checkpoint (`journal_id`, `next_usn`).
    ///
    /// # Errors
    ///
    /// Propagates any I/O error from writing to `w`.
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

        write_vec(w, &mut h, &self.dict_pool)?;
        write_vec(w, &mut h, &self.dict_off)?;
        write_vec(w, &mut h, &self.name_id)?;
        write_vec(w, &mut h, &self.orig_pool)?;
        write_vec(w, &mut h, &self.orig_off)?;
        write_vec(w, &mut h, &self.parent)?;
        write_vec(w, &mut h, &self.size_lo)?;
        // Overflow sizes as two parallel sections, id-sorted (deterministic
        // bytes for identical content; the map iterates in hash order).
        let mut ovf: Vec<(EntryId, u64)> = self.size_ovf.iter().map(|(&k, &v)| (k, v)).collect();
        ovf.sort_unstable();
        let ovf_ids: Vec<u32> = ovf.iter().map(|p| p.0).collect();
        let ovf_sizes: Vec<u64> = ovf.iter().map(|p| p.1).collect();
        write_vec(w, &mut h, &ovf_ids)?;
        write_vec(w, &mut h, &ovf_sizes)?;
        write_vec(w, &mut h, &self.mtime)?;
        write_vec(w, &mut h, &self.frn)?;
        write_vec(w, &mut h, &self.flag)?;
        write_vec(w, &mut h, &self.perm_name)?;
        w.write_all(&h.digest().to_le_bytes())
    }

    /// Load a snapshot; returns the index and the persisted (`journal_id`,
    /// `next_usn`) checkpoint. Any structural or checksum mismatch is an error
    /// — callers fall back to a full rescan.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`]: any underlying read error, or
    /// `InvalidData` on a bad magic, checksum mismatch, or any structural
    /// inconsistency in the (untrusted) stream.
    ///
    /// # Panics
    ///
    /// Panics only on an internal invariant violation: the fixed-size header
    /// conversions assume the 32-byte header buffer, which is always read in
    /// full before they run.
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
        let journal_id = u64::from_le_bytes(head[8..16].try_into().expect("32-byte header"));
        let next_usn = i64::from_le_bytes(head[16..24].try_into().expect("32-byte header"));
        let count = u64::from_le_bytes(head[24..32].try_into().expect("32-byte header")) as usize;

        let dict_pool: Vec<u8> = read_vec(r, &mut h)?;
        let dict_off: Vec<u32> = read_vec(r, &mut h)?;
        let name_id: Vec<u32> = read_vec(r, &mut h)?;
        let orig_pool: Vec<u8> = read_vec(r, &mut h)?;
        let orig_off: Vec<u32> = read_vec(r, &mut h)?;
        let parent: Vec<u32> = read_vec(r, &mut h)?;
        let size_lo: Vec<u32> = read_vec(r, &mut h)?;
        let ovf_ids: Vec<u32> = read_vec(r, &mut h)?;
        let ovf_sizes: Vec<u64> = read_vec(r, &mut h)?;
        let mtime: Vec<u32> = read_vec(r, &mut h)?;
        let frn: Vec<u64> = read_vec(r, &mut h)?;
        let flag: Vec<u8> = read_vec(r, &mut h)?;
        let perm_name: Vec<u32> = read_vec(r, &mut h)?;

        let mut digest = [0u8; 8];
        r.read_exact(&mut digest)?;
        if u64::from_le_bytes(digest) != h.digest() {
            return Err(bad("checksum mismatch"));
        }
        let columns_ok = [
            name_id.len(),
            orig_off.len(),
            parent.len(),
            size_lo.len(),
            mtime.len(),
            frn.len(),
            flag.len(),
            perm_name.len(),
        ]
        .iter()
        .all(|&l| l == count);
        if !columns_ok {
            return Err(bad("column length mismatch"));
        }
        // Name slices are untrusted input. The dictionary is gapless
        // (ADR-0033): `dict_off` must be non-decreasing and within the pool,
        // so name k spans `[dict_off[k], dict_off[k+1])` (pool end for the
        // last). A crafted out-of-order or past-the-pool offset would make
        // `lower_name` slice out of bounds.
        let dict_count = dict_off.len();
        for k in 0..dict_count {
            let off = dict_off[k] as usize;
            let end = dict_off.get(k + 1).map_or(dict_pool.len(), |&e| e as usize);
            if off > end || end > dict_pool.len() {
                return Err(bad("dict slice out of pool bounds"));
            }
        }
        // Then each entry: its `name_id` must index the dict, and its original
        // copy (when one exists) must fit using the dict-derived length (the
        // fold is length-preserving, ADR-0004).
        for i in 0..count {
            let nid = name_id[i] as usize;
            if nid >= dict_count {
                return Err(bad("name_id out of dict bounds"));
            }
            let len = {
                let off = dict_off[nid] as usize;
                dict_off
                    .get(nid + 1)
                    .map_or(dict_pool.len(), |&e| e as usize)
                    - off
            };
            let orig_ok = match orig_off[i] {
                u32::MAX => true,
                off => (off as usize)
                    .checked_add(len)
                    .is_some_and(|end| end <= orig_pool.len()),
            };
            if !orig_ok {
                return Err(bad("name slice out of pool bounds"));
            }
        }
        // Overflow pairs are untrusted too: every id must point at a
        // sentinel slot (strictly ascending — no duplicates), every
        // sentinel must have its pair, and the stored size must actually
        // need the overflow.
        if ovf_ids.len() != ovf_sizes.len() {
            return Err(bad("size overflow length mismatch"));
        }
        let sentinels = size_lo.iter().filter(|&&v| v == u32::MAX).count();
        if sentinels != ovf_ids.len() {
            return Err(bad("size overflow sentinel mismatch"));
        }
        for (i, (&id, &sz)) in ovf_ids.iter().zip(&ovf_sizes).enumerate() {
            let ascending = i == 0 || ovf_ids[i - 1] < id;
            if !ascending
                || (id as usize) >= count
                || size_lo[id as usize] != u32::MAX
                || sz < u32::MAX as u64
            {
                return Err(bad("size overflow pair invalid"));
            }
        }
        let size_ovf: rustc_hash::FxHashMap<u32, u64> =
            ovf_ids.into_iter().zip(ovf_sizes).collect();

        let frn_index = super::frn::FrnIndex::build(&frn, &flag);
        let mut tombstones = 0u32;
        // Lower bound: rename gaps aren't tombstoned and are lost here.
        let mut dead_name_bytes = 0u64;
        for (i, f) in flag.iter().enumerate() {
            if f & flags::TOMBSTONE != 0 {
                tombstones += 1;
                // Only the original copy is reclaimable per entry; folded
                // bytes are shared in the dictionary (ADR-0032).
                if orig_off[i] != u32::MAX {
                    let nid = name_id[i] as usize;
                    let off = dict_off[nid] as usize;
                    let end = dict_off
                        .get(nid + 1)
                        .map_or(dict_pool.len(), |&e| e as usize);
                    dead_name_bytes += (end - off) as u64;
                }
            }
        }

        Ok((
            Self {
                dict_pool,
                dict_off,
                name_id,
                orig_pool,
                orig_off,
                parent,
                size_lo,
                size_ovf,
                mtime,
                frn,
                flag,
                frn_index,
                perm_name,
                content_generation: 0,
                structural_generation: 0,
                dir_topology_generation: 0,
                tombstones,
                dead_name_bytes,
                dict_appends_since_dedup: 0,
                derived_cache: Mutex::new(None),
            },
            journal_id,
            next_usn,
        ))
    }

    /// Atomic save: write to `<path>.tmp`, then rename over the target.
    ///
    /// # Errors
    ///
    /// Propagates any I/O error from creating the parent directory, writing
    /// the temporary file, or renaming it over the target.
    pub fn save_to(
        &self,
        path: &std::path::Path,
        journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        use std::io::Write;

        let tmp = path.with_extension("fmfidx.tmp");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        {
            let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            self.write_snapshot(&mut w, journal_id, next_usn)?;
            w.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Open `path` and load the snapshot it holds.
    ///
    /// # Errors
    ///
    /// Propagates any I/O error from opening the file, or any error from
    /// [`Self::read_snapshot`] (a corrupt or incompatible snapshot).
    pub fn load_from(path: &std::path::Path) -> std::io::Result<(Self, u64, i64)> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        Self::read_snapshot(&mut r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::index::testutil::{TestDir, build_sample};

    #[test]
    fn snapshot_roundtrip_preserves_everything() {
        let mut idx = build_sample();
        idx.delete(60); // include a tombstone
        // A ≥4GiB file exercises the size-overflow sections.
        let first_new = idx.len() as u32;
        let huge = crate::index::testutil::u16s("huge.iso");
        let huge_id = idx.upsert(&crate::index::testutil::raw(
            777,
            50,
            &huge,
            false,
            (5u64 << 30) + 123,
            1,
        ));
        idx.merge_new_into_permutations(first_new);

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
        assert_eq!(loaded.name_permutation(), idx.name_permutation());
        assert_eq!(loaded.size(huge_id), (5u64 << 30) + 123);
    }

    #[test]
    fn snapshot_size_overflow_inconsistencies_are_rejected() {
        // An overflow pair pointing at a non-sentinel slot.
        let mut sections = valid_sections();
        sections[7] = vec![0u8; 4]; // ovf id 0, but size_lo[0] != MAX
        sections[8] = 0x00FF_FFFF_FFFF_u64.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("sentinel mismatch"), "{err}");

        // A sentinel slot without its overflow pair.
        let mut sections = valid_sections();
        sections[6] = u32::MAX.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("sentinel mismatch"), "{err}");

        // Pair present but the stored size doesn't need the overflow.
        let mut sections = valid_sections();
        sections[6] = u32::MAX.to_le_bytes().to_vec();
        sections[7] = vec![0u8; 4];
        sections[8] = 42u64.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("pair invalid"), "{err}");

        // Mismatched ids/sizes section lengths.
        let mut sections = valid_sections();
        sections[6] = u32::MAX.to_le_bytes().to_vec();
        sections[7] = vec![0u8; 4];
        let err = read_crafted(sections);
        assert!(err.to_string().contains("length mismatch"), "{err}");
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

    /// Checksum-valid stream built from raw section bytes, so structural
    /// validation (not the digest) is what must reject it.
    fn craft_stream(count: u64, sections: &[&[u8]]) -> Vec<u8> {
        let mut h = xxhash_rust::xxh64::Xxh64::new(0);
        let mut buf = Vec::new();
        buf.extend_from_slice(SNAPSHOT_MAGIC);
        buf.extend_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&2i64.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        h.update(&buf);
        for s in sections {
            write_vec(&mut buf, &mut h, s).unwrap();
        }
        let digest = h.digest().to_le_bytes();
        buf.extend_from_slice(&digest);
        buf
    }

    /// Section byte sizes for a structurally valid count=1 snapshot, in read
    /// order (ADR-0033 / FMFIDX07): `dict_pool`, `dict_off`, `name_id`,
    /// `orig_pool` (empty), `orig_off` (sentinel), parent, `size_lo`,
    /// size-overflow ids/sizes (empty), mtime, frn, flag, `perm_name`.
    fn valid_sections() -> Vec<Vec<u8>> {
        vec![
            b"a".to_vec(),                   // dict_pool (one distinct name "a", len 1)
            0u32.to_le_bytes().to_vec(),     // dict_off [0] (gapless; len = pool end − 0)
            0u32.to_le_bytes().to_vec(),     // name_id [0] (entry 0 → name 0)
            Vec::new(),                      // orig_pool (fold-identical)
            u32::MAX.to_le_bytes().to_vec(), // orig_off (sentinel)
            vec![0u8; 4],                    // parent
            vec![0u8; 4],                    // size_lo (1 × u32)
            Vec::new(),                      // size overflow ids (none)
            Vec::new(),                      // size overflow sizes (none)
            vec![0u8; 4],                    // mtime (1 × u32)
            vec![0u8; 8],                    // frn
            vec![0u8; 1],                    // flag
            vec![0u8; 4],                    // perm_name
        ]
    }

    /// `unwrap_err` needs `Debug` on the Ok side; `VolumeIndex` has none.
    fn expect_load_err(buf: &[u8]) -> std::io::Error {
        match VolumeIndex::read_snapshot(&mut &*buf) {
            Ok(_) => panic!("corrupted snapshot must not load"),
            Err(e) => e,
        }
    }

    fn read_crafted(sections: Vec<Vec<u8>>) -> std::io::Error {
        let refs: Vec<&[u8]> = sections.iter().map(Vec::as_slice).collect();
        expect_load_err(&craft_stream(1, &refs))
    }

    #[test]
    fn snapshot_wrong_magic_is_rejected() {
        let idx = build_sample();
        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 1).unwrap();
        buf[..8].copy_from_slice(b"FMFIDX01"); // previous format version
        let err = expect_load_err(&buf);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("magic"), "{err}");
    }

    #[test]
    fn snapshot_crafted_stream_sanity_loads() {
        // Control: the crafted-stream helper itself round-trips, so the
        // corruption tests below fail for the corruption, not the harness.
        let refs: Vec<Vec<u8>> = valid_sections();
        let refs: Vec<&[u8]> = refs.iter().map(Vec::as_slice).collect();
        let buf = craft_stream(1, &refs);
        let (idx, journal_id, next_usn) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
        assert_eq!((journal_id, next_usn), (1, 2));
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn snapshot_column_count_mismatch_is_rejected_not_panic() {
        // name_id carries 2 entries while the header says count=1.
        let mut sections = valid_sections();
        sections[2] = vec![0u8; 8];
        let err = read_crafted(sections);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("column length"), "{err}");
    }

    #[test]
    fn snapshot_misaligned_section_is_rejected_not_panic() {
        // 7 bytes cannot be a u32 column.
        let mut sections = valid_sections();
        sections[2] = vec![0u8; 7];
        let err = read_crafted(sections);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("section size"), "{err}");
    }

    #[test]
    fn snapshot_out_of_bounds_name_slices_are_rejected() {
        // A dict offset past the (1-byte) dict pool — the gapless validator
        // rejects it before any slice (ADR-0033).
        let mut sections = valid_sections();
        sections[1] = 5u32.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("out of pool bounds"), "{err}");

        // A non-sentinel orig_off pointing past the (empty) orig pool.
        let mut sections = valid_sections();
        sections[4] = 0u32.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("out of pool bounds"), "{err}");

        // Control: a real orig byte at offset 0 loads fine and reads back.
        let mut sections = valid_sections();
        sections[3] = b"A".to_vec();
        sections[4] = 0u32.to_le_bytes().to_vec();
        let refs: Vec<&[u8]> = sections.iter().map(Vec::as_slice).collect();
        let buf = craft_stream(1, &refs);
        let (idx, _, _) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
        assert_eq!(idx.name(0), b"A");
        assert_eq!(idx.lower_name(0), b"a");
    }

    #[test]
    fn snapshot_lying_length_prefix_errors_without_huge_allocation() {
        // A corrupt section length (here 2^60) must come back as Err, not
        // allocate the claimed size and abort the process.
        let idx = build_sample();
        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 1).unwrap();
        buf[32..40].copy_from_slice(&(1u64 << 60).to_le_bytes());
        assert!(VolumeIndex::read_snapshot(&mut buf.as_slice()).is_err());
    }

    #[test]
    fn truncated_snapshot_errors_at_every_cut_point() {
        let mut idx = build_sample();
        idx.delete(60); // include a tombstone for variety
        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 1).unwrap();
        assert!(VolumeIndex::read_snapshot(&mut buf.as_slice()).is_ok());
        for cut in 0..buf.len() {
            assert!(
                VolumeIndex::read_snapshot(&mut &buf[..cut]).is_err(),
                "cut at {cut} must error, not panic or succeed"
            );
        }
    }

    #[test]
    fn save_to_leaves_no_tmp_file_and_overwrites() {
        let dir = TestDir::new();
        let target = dir.join("vol_c.fmfidx");
        let idx = build_sample();
        idx.save_to(&target, 7, 8).unwrap();
        idx.save_to(&target, 9, 10).unwrap(); // overwrite an existing target

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["vol_c.fmfidx"], "no .tmp or stray files");

        let (loaded, journal_id, next_usn) = VolumeIndex::load_from(&target).unwrap();
        assert_eq!((journal_id, next_usn), (9, 10));
        assert_eq!(loaded.len(), idx.len());
    }
}

#[cfg(test)]
mod proptests {
    //! Round-trip property: any index `write_snapshot` then `read_snapshot`
    //! reproduces the same observable state and checkpoint — the example-based
    //! tests above pin specific corruptions, this pins fidelity over the whole
    //! generated space (names spanning ASCII/multibyte/surrogate, sizes that
    //! straddle the 4 GiB `size_lo`/`size_ovf` split, real-FILETIME mtimes).

    use proptest::prelude::*;

    use super::*;
    use crate::index::{Frn, RawEntry, VolumeIndexBuilder};

    /// 4 GiB: the boundary above which a size spills from the `size_lo` u32
    /// column into the `size_ovf` side map (two serialized sections).
    const OVF: u64 = 4 << 30;

    const FRAGMENTS: &[&str] = &[
        "a", "Re", "report", "日本", "𠮷", ".rs", ".TXT", "x", "main",
    ];

    #[derive(Debug, Clone)]
    struct Ent {
        name: String,
        is_dir: bool,
        size: u64,
        mtime: i64,
    }

    fn ent_strategy() -> impl Strategy<Value = Ent> {
        const EPOCH: i64 = crate::query::dates::FILETIME_UNIX_EPOCH;
        (
            proptest::collection::vec(0usize..FRAGMENTS.len(), 1..=3),
            any::<bool>(),
            // Straddle the overflow boundary so both the inline column and the
            // side map are serialized across the generated cases.
            prop_oneof![0u64..OVF, OVF..(64u64 << 30)],
            EPOCH..(EPOCH + 10_000i64 * 864_000_000_000),
        )
            .prop_map(|(parts, is_dir, size, mtime)| Ent {
                name: parts.iter().map(|&i| FRAGMENTS[i]).collect(),
                is_dir,
                size,
                mtime,
            })
    }

    /// Build an index whose records are `10, 11, …`, all under the root.
    fn build(entries: &[Ent]) -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        for (i, e) in entries.iter().enumerate() {
            let units: Vec<u16> = e.name.encode_utf16().collect();
            let record = i as u64 + 10;
            b.push(RawEntry {
                parent_frn: Frn(5),
                frn: Frn((1u64 << 48) | record),
                name_utf16: &units,
                is_dir: e.is_dir,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: e.size,
                mtime: e.mtime,
            });
        }
        b.finish()
    }

    fn assert_same(a: &VolumeIndex, b: &VolumeIndex, records: usize) {
        assert_eq!(a.len(), b.len());
        assert_eq!(a.live_len(), b.live_len());
        for i in 0..records as u64 {
            let rec = i + 10;
            let ia = a.entry_by_record(rec).expect("record present in source");
            let ib = b.entry_by_record(rec).expect("record present in loaded");
            assert_eq!(a.name(ia), b.name(ib), "name for record {rec}");
            assert_eq!(a.size(ia), b.size(ib), "size for record {rec}");
            assert_eq!(a.mtime(ia), b.mtime(ib), "mtime for record {rec}");
            let (mut pa, mut pb) = (Vec::new(), Vec::new());
            a.append_path(ia, &mut pa);
            b.append_path(ib, &mut pb);
            assert_eq!(pa, pb, "path for record {rec}");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn snapshot_round_trips_observable_state_and_checkpoint(
            entries in proptest::collection::vec(ent_strategy(), 0..12),
            journal_id in any::<u64>(),
            next_usn in any::<i64>(),
        ) {
            let idx = build(&entries);
            let mut buf = Vec::new();
            idx.write_snapshot(&mut buf, journal_id, next_usn).unwrap();
            let (loaded, gj, gu) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
            prop_assert_eq!((gj, gu), (journal_id, next_usn), "checkpoint must round-trip");
            assert_same(&idx, &loaded, entries.len());
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Robustness: `read_snapshot` parses an *untrusted* stream with unsafe
        /// POD reads (`read_vec`), so arbitrary byte mutations or truncations of
        /// a valid snapshot must come back as `Ok`/`Err` — never a panic,
        /// over-read, or runaway allocation. Mutations to a section's length
        /// prefix exercise the `try_reserve`/EOF guards that sit *before* the
        /// trailing checksum check. (Linux-buildable fuzzing of this path is
        /// blocked by fmf-core's Windows-only deps — see fuzz/README.md; this is
        /// the in-tree, Windows-runnable stand-in.)
        #[test]
        fn read_snapshot_survives_arbitrary_mutation_without_panicking(
            entries in proptest::collection::vec(ent_strategy(), 1..6),
            overwrites in proptest::collection::vec(
                (any::<prop::sample::Index>(), any::<u8>()), 0..10),
            truncate_to in any::<prop::sample::Index>(),
        ) {
            let mut buf = Vec::new();
            build(&entries).write_snapshot(&mut buf, 1, 2).unwrap();
            for (at, byte) in &overwrites {
                let pos = at.index(buf.len());
                buf[pos] = *byte;
            }
            let cut = truncate_to.index(buf.len() + 1);
            let slice = &buf[..cut];

            // The contract: returns without panicking. A successful load passed
            // full structural + checksum validation, so every entry is safely
            // walkable (no OOB name/path access).
            if let Ok((loaded, _, _)) = VolumeIndex::read_snapshot(&mut &slice[..]) {
                let mut p = Vec::new();
                for id in 0..loaded.len() as u32 {
                    let _ = loaded.name(id);
                    if loaded.is_live(id) {
                        p.clear();
                        loaded.append_path(id, &mut p);
                    }
                }
            }
        }
    }

    /// Concrete vacuity companion: a ≥ 4 GiB size really does survive the
    /// `size_ovf` side-map path (so the property above is not green merely
    /// because every generated size stayed in the u32 column).
    #[test]
    fn overflow_size_survives_the_round_trip() {
        let idx = build(&[Ent {
            name: "huge.iso".into(),
            is_dir: false,
            size: (7u64 << 30) + 123,
            mtime: 0,
        }]);
        let mut buf = Vec::new();
        idx.write_snapshot(&mut buf, 1, 2).unwrap();
        let (loaded, _, _) = VolumeIndex::read_snapshot(&mut buf.as_slice()).unwrap();
        let id = loaded.entry_by_record(10).unwrap();
        assert_eq!(loaded.size(id), (7u64 << 30) + 123);
    }
}
