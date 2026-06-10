// ── Snapshot persistence (.fmfidx) ──────────────────────────────────────
//
// Header (magic, version, journal checkpoint) + raw little-endian column
// dumps + trailing xxhash64. Machine-local cache only — corruption or any
// mismatch falls back to a full rescan, so the format favors speed over
// portability (docs/ARCHITECTURE.md).

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use super::{EntryId, VolumeIndex, flags, masked};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SortKey;
    use crate::index::testutil::build_sample;

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
}
