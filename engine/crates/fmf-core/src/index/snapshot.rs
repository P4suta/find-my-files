// ── Snapshot persistence (.fmfidx) ──────────────────────────────────────
//
// Header (magic, version, journal checkpoint) + raw little-endian column
// dumps + trailing xxhash64. Machine-local cache only — corruption or any
// mismatch falls back to a full rescan, so the format favors speed over
// portability (docs/ARCHITECTURE.md).

use parking_lot::Mutex;

use super::{EntryId, VolumeIndex, flags};

// 02: flag byte gained HIDDEN/SYSTEM/EXCLUDED bits — older snapshots must
// trigger a full rescan rather than load with wrong semantics.
// 03: perm_size/perm_mtime sections dropped (lazy derived caches now) and
//     the size column split into u32 + an overflow id/size pair list.
const SNAPSHOT_MAGIC: &[u8; 8] = b"FMFIDX03";

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

fn read_vec<T: Copy, R: std::io::Read>(
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
    // The length prefix is untrusted input: a corrupt value must come back
    // as Err (→ full-rescan fallback), not abort the process. try_reserve
    // turns an absurd claim into a clean Err; a plausible-but-lying length
    // then fails at read_exact EOF and the buffer drops. Reserving once and
    // reading in 8MiB strides replaces the old 1MiB grow-loop, whose
    // repeated reallocations dominated large-column load time.
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
        let size_lo: Vec<u32> = read_vec(r, &mut h)?;
        let ovf_ids: Vec<u32> = read_vec(r, &mut h)?;
        let ovf_sizes: Vec<u64> = read_vec(r, &mut h)?;
        let mtime: Vec<i64> = read_vec(r, &mut h)?;
        let frn: Vec<u64> = read_vec(r, &mut h)?;
        let flag: Vec<u8> = read_vec(r, &mut h)?;
        let perm_name: Vec<u32> = read_vec(r, &mut h)?;

        let mut digest = [0u8; 8];
        r.read_exact(&mut digest)?;
        if u64::from_le_bytes(digest) != h.digest() {
            return Err(bad("checksum mismatch"));
        }
        let columns_ok = [
            name_off.len(),
            name_len.len(),
            parent.len(),
            size_lo.len(),
            mtime.len(),
            frn.len(),
            flag.len(),
            perm_name.len(),
        ]
        .iter()
        .all(|&l| l == count);
        if !columns_ok || name_pool.len() != lower_pool.len() {
            return Err(bad("column length mismatch"));
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

        // One parallel sort instead of the million serial hashmap inserts
        // this rebuild used to be.
        let frn_index = super::frn::FrnIndex::build(&frn, &flag);
        let mut tombstones = 0u32;
        // Lower bound: rename gaps aren't tombstoned and are lost here.
        let mut dead_name_bytes = 0u64;
        for (i, f) in flag.iter().enumerate() {
            if f & flags::TOMBSTONE != 0 {
                tombstones += 1;
                dead_name_bytes += name_len[i] as u64;
            }
        }

        Ok((
            Self {
                name_pool,
                lower_pool,
                name_off,
                name_len,
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::index::testutil::build_sample;

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
        sections[6] = vec![0u8; 4]; // ovf id 0, but size_lo[0] != MAX
        sections[7] = 0x00FF_FFFF_FFFF_u64.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("sentinel mismatch"), "{err}");

        // A sentinel slot without its overflow pair.
        let mut sections = valid_sections();
        sections[5] = u32::MAX.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("sentinel mismatch"), "{err}");

        // Pair present but the stored size doesn't need the overflow.
        let mut sections = valid_sections();
        sections[5] = u32::MAX.to_le_bytes().to_vec();
        sections[6] = vec![0u8; 4];
        sections[7] = 42u64.to_le_bytes().to_vec();
        let err = read_crafted(sections);
        assert!(err.to_string().contains("pair invalid"), "{err}");

        // Mismatched ids/sizes section lengths.
        let mut sections = valid_sections();
        sections[5] = u32::MAX.to_le_bytes().to_vec();
        sections[6] = vec![0u8; 4];
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

    /// Section byte sizes for a structurally valid count=1 snapshot, in
    /// read order: pools ×2, name_off, name_len, parent, size_lo,
    /// size-overflow ids/sizes (empty), mtime, frn, flag, perm_name.
    fn valid_sections() -> Vec<Vec<u8>> {
        vec![
            b"a".to_vec(), // name_pool
            b"a".to_vec(), // lower_pool
            vec![0u8; 4],  // name_off (1 × u32)
            vec![0u8; 2],  // name_len (1 × u16)
            vec![0u8; 4],  // parent
            vec![0u8; 4],  // size_lo (1 × u32)
            Vec::new(),    // size overflow ids (none)
            Vec::new(),    // size overflow sizes (none)
            vec![0u8; 8],  // mtime
            vec![0u8; 8],  // frn
            vec![0u8; 1],  // flag
            vec![0u8; 4],  // perm_name
        ]
    }

    /// `unwrap_err` needs `Debug` on the Ok side; VolumeIndex has none.
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
        // name_off carries 2 entries while the header says count=1.
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
    fn snapshot_pool_length_mismatch_is_rejected() {
        // name_pool and lower_pool must be byte-for-byte the same length.
        let mut sections = valid_sections();
        sections[0] = b"ab".to_vec();
        let err = read_crafted(sections);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("column length"), "{err}");
    }

    #[test]
    fn snapshot_lying_length_prefix_errors_without_huge_allocation() {
        // A corrupt section length (here 2^60) must come back as Err — the
        // old code pre-allocated the claimed size before any validation,
        // which aborts the process instead of falling back to a rescan.
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
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fmf-core-snap-test-{}-{unique}",
            std::process::id()
        ));
        let target = dir.join("vol_c.fmfidx");
        let idx = build_sample();
        idx.save_to(&target, 7, 8).unwrap();
        idx.save_to(&target, 9, 10).unwrap(); // overwrite an existing target

        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["vol_c.fmfidx"], "no .tmp or stray files");

        let (loaded, journal_id, next_usn) = VolumeIndex::load_from(&target).unwrap();
        assert_eq!((journal_id, next_usn), (9, 10));
        assert_eq!(loaded.len(), idx.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
