//! Raw volume access: `\\.\C:`-style handles, the NTFS update-sequence
//! fixup, and the logical→physical run map of the $MFT data stream.

use ntfs_reader::api::NtfsAttributeType;
use ntfs_reader::errors::NtfsReaderError;
use ntfs_reader::file::NtfsFile;
use ntfs_reader::mft::Mft;
use ntfs_reader::volume::Volume;

const SECTOR: usize = 512;

/// Logical-byte → physical-byte mapping of the $MFT data stream.
pub(super) struct RunMap {
    /// (logical start, physical start, length) — all bytes.
    pub(super) runs: Vec<(u64, u64, u64)>,
}

impl RunMap {
    fn from_data_runs(runs: &[ntfs_reader::attribute::DataRun]) -> Self {
        use ntfs_reader::attribute::DataRun;
        let mut v = Vec::with_capacity(runs.len());
        let mut logical = 0u64;
        for r in runs {
            match r {
                DataRun::Data { lcn, length } => {
                    v.push((logical, *lcn, *length));
                    logical += length;
                }
                DataRun::Sparse { length } => logical += length,
            }
        }
        Self { runs: v }
    }

    /// Physical offset and remaining contiguous bytes at `logical`.
    pub(super) fn physical(&self, logical: u64) -> Option<(u64, u64)> {
        // Runs are few (usually < 100); linear is fine.
        self.runs
            .iter()
            .find(|(ls, _, len)| logical >= *ls && logical < ls + len)
            .map(|(ls, ph, len)| (ph + (logical - ls), ls + len - logical))
    }
}

pub(super) fn open_raw_volume(volume_path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(volume_path)
}

/// Apply the NTFS update sequence array in place. Returns false when the
/// sector check bytes don't match (torn/corrupt record).
pub(super) fn apply_fixup(data: &mut [u8]) -> bool {
    if data.len() < 48 {
        return false;
    }
    let uso = u16::from_le_bytes([data[4], data[5]]) as usize;
    let usl = u16::from_le_bytes([data[6], data[7]]) as usize;
    if usl < 2 || uso + usl * 2 > data.len() {
        return false;
    }
    let usn = [data[uso], data[uso + 1]];
    let mut sector_off = SECTOR - 2;
    for i in 1..usl {
        let usa_off = uso + i * 2;
        if sector_off + 2 > data.len() {
            break;
        }
        if data[sector_off] != usn[0] || data[sector_off + 1] != usn[1] {
            return false;
        }
        data[sector_off] = data[usa_off];
        data[sector_off + 1] = data[usa_off + 1];
        sector_off += SECTOR;
    }
    true
}

/// Volume geometry + the $MFT data-run map — the bootstrap shared by the
/// full scan and the I/O probe (record 0 → the $MFT's own data runs).
pub(super) fn mft_layout(volume_path: &str) -> Result<(usize, u64, RunMap), NtfsReaderError> {
    let volume = Volume::new(volume_path)?;
    let record_size = volume.file_record_size as usize;
    let mut reader = ntfs_reader::aligned_reader::open_volume(std::path::Path::new(volume_path))
        .map_err(NtfsReaderError::from)?;
    let rec0 = Mft::get_record_fs(&mut reader, volume.file_record_size, volume.mft_position)?;
    let f0 = NtfsFile::new(0, &rec0);
    let data_attr = f0
        .get_attribute(NtfsAttributeType::Data)
        .ok_or_else(|| NtfsReaderError::MissingMftAttribute("Data".to_string()))?;
    let (size, runs) = data_attr.get_nonresident_data_runs(&volume)?;
    Ok((record_size, size, RunMap::from_data_runs(&runs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `len`-byte record with an update-sequence array at `uso`
    /// carrying `usn` plus `fixups` (the bytes that belong at each sector
    /// tail), and write the `usn` sentinel into each sector tail so a correct
    /// `apply_fixup` succeeds and restores the `fixups`.
    fn record_with_usa(len: usize, uso: usize, usn: u16, fixups: &[u16]) -> Vec<u8> {
        let mut r = vec![0u8; len];
        let usl = (fixups.len() + 1) as u16;
        r[4..6].copy_from_slice(&(uso as u16).to_le_bytes());
        r[6..8].copy_from_slice(&usl.to_le_bytes());
        r[uso..uso + 2].copy_from_slice(&usn.to_le_bytes());
        for (i, f) in fixups.iter().enumerate() {
            let off = uso + (i + 1) * 2;
            r[off..off + 2].copy_from_slice(&f.to_le_bytes());
            let tail = (i + 1) * SECTOR - 2;
            r[tail..tail + 2].copy_from_slice(&usn.to_le_bytes());
        }
        r
    }

    #[test]
    fn rejects_a_buffer_too_small_for_a_header() {
        assert!(!apply_fixup(&mut [0u8; 47]));
    }

    #[test]
    fn rejects_an_update_sequence_length_below_two() {
        let mut r = vec![0u8; 1024];
        r[4..6].copy_from_slice(&48u16.to_le_bytes()); // uso
        r[6..8].copy_from_slice(&1u16.to_le_bytes()); // usl = 1 (no fixups)
        assert!(!apply_fixup(&mut r));
    }

    #[test]
    fn rejects_a_usa_that_runs_past_the_buffer() {
        let mut r = vec![0u8; 1024];
        r[4..6].copy_from_slice(&1020u16.to_le_bytes()); // uso near the end
        r[6..8].copy_from_slice(&8u16.to_le_bytes()); // uso + usl*2 > len
        assert!(!apply_fixup(&mut r));
    }

    #[test]
    fn applies_the_update_sequence_and_restores_sector_tails() {
        // Two sectors ⇒ two fixups; the tails currently hold the sentinel and
        // must come back as 0xAAAA and 0xBBBB after the fixup.
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        assert!(apply_fixup(&mut r));
        assert_eq!(u16::from_le_bytes([r[510], r[511]]), 0xAAAA);
        assert_eq!(u16::from_le_bytes([r[1022], r[1023]]), 0xBBBB);
    }

    #[test]
    fn rejects_a_torn_record_whose_sector_tail_lost_the_sentinel() {
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        // Corrupt the second sector tail so it no longer matches the USN.
        r[1022] = 0x99;
        assert!(!apply_fixup(&mut r));
    }
}
