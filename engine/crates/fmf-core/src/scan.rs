//! Streaming $MFT scanner (perf plan Workstream C).
//!
//! Replaces the whole-$MFT-in-RAM approach: the $MFT's data runs are read in
//! 16MiB aligned chunks through our own volume handle (large sequential
//! reads run at device speed instead of ntfs-reader's small buffered ones),
//! records are fixed up and parsed per chunk, and the buffer is reused —
//! peak RAM drops from "size of $MFT" to one chunk. ntfs-reader still
//! provides the bootstrap (boot-sector geometry + record 0's data runs) and
//! the per-record attribute parsing types.

use std::time::Instant;

use ntfs_reader::api::{
    FIRST_NORMAL_RECORD, NtfsAttributeListEntry, NtfsAttributeType, NtfsFileName,
    NtfsFileNamespace, ROOT_RECORD,
};
use ntfs_reader::errors::NtfsReaderError;
use ntfs_reader::file::NtfsFile;
use ntfs_reader::mft::Mft;
use ntfs_reader::volume::Volume;

use crate::index::{RawEntry, VolumeIndex, VolumeIndexBuilder};
use crate::mft::{MftError, ScanStats, peak_working_set, pick_name};

const SCAN_CHUNK: usize = 16 << 20;
const SECTOR: usize = 512;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Logical-byte → physical-byte mapping of the $MFT data stream.
struct RunMap {
    /// (logical start, physical start, length) — all bytes.
    runs: Vec<(u64, u64, u64)>,
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
        RunMap { runs: v }
    }

    /// Physical offset and remaining contiguous bytes at `logical`.
    fn physical(&self, logical: u64) -> Option<(u64, u64)> {
        // Runs are few (usually < 100); linear is fine.
        self.runs
            .iter()
            .find(|(ls, _, len)| logical >= *ls && logical < ls + len)
            .map(|(ls, ph, len)| (ph + (logical - ls), ls + len - logical))
    }
}

fn open_raw_volume(volume_path: &str) -> std::io::Result<std::fs::File> {
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
fn apply_fixup(data: &mut [u8]) -> bool {
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

/// Random access to single records for the deferred attribute-list pass.
struct RecordReader<'a> {
    file: std::fs::File,
    map: &'a RunMap,
    record_size: usize,
    buf: Vec<u8>,
}

impl RecordReader<'_> {
    fn read_record(&mut self, number: u64) -> Option<&[u8]> {
        use std::io::{Read, Seek, SeekFrom};
        let logical = number * self.record_size as u64;
        let (phys, contig) = self.map.physical(logical)?;
        if (contig as usize) < self.record_size {
            return None;
        }
        self.buf.resize(self.record_size, 0);
        self.file.seek(SeekFrom::Start(phys)).ok()?;
        self.file.read_exact(&mut self.buf).ok()?;
        if !NtfsFile::is_valid(&self.buf) || !apply_fixup(&mut self.buf) {
            return None;
        }
        Some(&self.buf)
    }
}

/// Resolve the display name of a record whose $FILE_NAME lives in extension
/// records (resident $ATTRIBUTE_LIST → referenced records). Mirrors
/// ntfs-reader's get_best_file_name without needing the whole MFT in RAM.
fn resolve_attr_list_name(base: &NtfsFile, rr: &mut RecordReader) -> Option<NtfsFileName> {
    let attr = base.get_attribute(NtfsAttributeType::AttributeList)?;
    if attr.header.is_non_resident != 0 {
        return None; // rare; counted as skipped
    }
    let header = attr.resident_header()?;
    let data = attr.data();
    let start = header.value_offset as usize;
    let end = start.checked_add(header.value_length as usize)?;
    if end > data.len() {
        return None;
    }
    let list = &data[start..end];

    let mut best: Option<NtfsFileName> = None;
    let mut off = 0usize;
    while off + size_of::<NtfsAttributeListEntry>() <= list.len() {
        let entry = unsafe { &*(list.as_ptr().add(off) as *const NtfsAttributeListEntry) };
        let len = entry.length as usize;
        if len < size_of::<NtfsAttributeListEntry>() || off + len > list.len() {
            break;
        }
        if entry.type_id == NtfsAttributeType::FileName as u32 {
            let target = entry.reference();
            if target != base.number
                && let Some(bytes) = rr.read_record(target)
            {
                let f = NtfsFile::new(target, bytes);
                if let Some(name) = pick_name(&f) {
                    let ns = name.header.namespace;
                    if ns == NtfsFileNamespace::Win32 as u8
                        || ns == NtfsFileNamespace::Win32AndDos as u8
                    {
                        return Some(name);
                    }
                    if best.is_none() {
                        best = Some(name);
                    }
                }
            }
        }
        off += len.next_multiple_of(8);
    }
    best
}

fn push_record(
    b: &mut VolumeIndexBuilder,
    stats: &mut ScanStats,
    f: &NtfsFile,
    name: &NtfsFileName,
) {
    let name_data = name.data;
    let name_len = name.header.name_length as usize;
    let parent_record = name.header.parent_directory_reference;

    let mut size = 0u64;
    let mut mtime = 0i64;
    let mut is_reparse = false;
    let mut is_hidden = false;
    let mut is_system = false;
    f.attributes(|att| {
        if att.header.type_id == NtfsAttributeType::StandardInformation as u32 {
            if let Some(si) = att.as_standard_info() {
                mtime = si.modification_time as i64;
                is_reparse = si.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
                is_hidden = si.file_attributes & FILE_ATTRIBUTE_HIDDEN != 0;
                is_system = si.file_attributes & FILE_ATTRIBUTE_SYSTEM != 0;
            }
        } else if att.header.type_id == NtfsAttributeType::Data as u32 {
            if att.header.is_non_resident == 0 {
                if let Some(h) = att.resident_header() {
                    size = h.value_length as u64;
                }
            } else if let Some(h) = att.nonresident_header() {
                size = h.data_size;
            }
        }
    });

    if f.is_directory() {
        stats.dirs += 1;
    } else {
        stats.files += 1;
    }
    b.push(RawEntry {
        record: f.number,
        parent_record,
        frn: f.reference_number(),
        name_utf16: &name_data[..name_len],
        is_dir: f.is_directory(),
        is_reparse,
        is_hidden,
        is_system,
        size,
        mtime,
    });
}

/// Full initial scan: stream the volume's $MFT and build the in-memory
/// index. `drive` is a drive letter spec like `C:`.
pub fn scan_volume(drive: &str) -> Result<(VolumeIndex, ScanStats), MftError> {
    use std::io::{Read, Seek, SeekFrom};

    let drive = drive.trim_end_matches(['\\', '/']);
    let volume_path = format!(r"\\.\{drive}");
    let mut stats = ScanStats {
        volume: drive.to_string(),
        ..Default::default()
    };

    let t0 = Instant::now();
    let volume = Volume::new(&volume_path).map_err(|e| match e {
        NtfsReaderError::ElevationError => MftError::NotElevated,
        other => MftError::Ntfs(other),
    })?;
    let record_size = volume.file_record_size as usize;

    // Bootstrap: record 0 → the $MFT's own data runs.
    let (data_size, runmap) = {
        let mut reader =
            ntfs_reader::aligned_reader::open_volume(std::path::Path::new(&volume_path))
                .map_err(NtfsReaderError::from)?;
        let rec0 = Mft::get_record_fs(&mut reader, volume.file_record_size, volume.mft_position)?;
        let f0 = NtfsFile::new(0, &rec0);
        let data_attr = f0
            .get_attribute(NtfsAttributeType::Data)
            .ok_or_else(|| NtfsReaderError::MissingMftAttribute("Data".to_string()))?;
        let (size, runs) = data_attr.get_nonresident_data_runs(&volume)?;
        (size, RunMap::from_data_runs(&runs))
    };
    stats.mft_bytes = data_size;

    let mut file = open_raw_volume(&volume_path).map_err(NtfsReaderError::from)?;
    let mut b = VolumeIndexBuilder::new(drive, ROOT_RECORD);
    let mut deferred: Vec<(u64, Box<[u8]>)> = Vec::new();
    let mut buf = vec![0u8; SCAN_CHUNK];
    let mut read_time = std::time::Duration::ZERO;

    let mut logical = 0u64;
    while logical < data_size {
        let Some((phys, contig)) = runmap.physical(logical) else {
            // Sparse hole: no records here.
            logical += record_size as u64;
            continue;
        };
        let want = SCAN_CHUNK
            .min(contig as usize)
            .min((data_size - logical) as usize)
            / record_size
            * record_size;
        if want == 0 {
            logical += record_size as u64;
            continue;
        }
        let tr = Instant::now();
        file.seek(SeekFrom::Start(phys))
            .map_err(NtfsReaderError::from)?;
        file.read_exact(&mut buf[..want])
            .map_err(NtfsReaderError::from)?;
        read_time += tr.elapsed();

        for off in (0..want).step_by(record_size) {
            let number = (logical + off as u64) / record_size as u64;
            if number < FIRST_NORMAL_RECORD {
                continue; // metafiles; the builder seeds the root itself
            }
            let rec = &mut buf[off..off + record_size];
            if !NtfsFile::is_valid(rec) {
                continue;
            }
            if !apply_fixup(rec) {
                stats.corrupt_records += 1;
                continue;
            }
            let f = NtfsFile::new(number, rec);
            if !f.is_used() {
                continue;
            }
            if { f.header.base_reference } & 0x0000_FFFF_FFFF_FFFF != 0 {
                stats.extension_records += 1;
                continue;
            }

            let Some(name) = pick_name(&f) else {
                if f.get_attribute(NtfsAttributeType::AttributeList).is_some() {
                    deferred.push((number, rec.to_vec().into_boxed_slice()));
                } else {
                    stats.skipped_no_name += 1;
                }
                continue;
            };
            push_record(&mut b, &mut stats, &f, &name);
        }
        logical += want as u64;
    }
    stats.elapsed_mft_load_ms = read_time.as_millis() as u64;

    // Deferred pass: names hiding behind $ATTRIBUTE_LIST (~tens of
    // thousands on a real C:) resolved with targeted single-record reads.
    let mut rr = RecordReader {
        file,
        map: &runmap,
        record_size,
        buf: Vec::new(),
    };
    for (number, bytes) in &deferred {
        let f = NtfsFile::new(*number, bytes);
        match resolve_attr_list_name(&f, &mut rr) {
            Some(name) => push_record(&mut b, &mut stats, &f, &name),
            None => stats.deferred_unresolved += 1,
        }
    }

    // Degradations are normal in small numbers; make them visible either way.
    if stats.corrupt_records > 0 {
        tracing::warn!(volume = %drive, count = stats.corrupt_records, "corrupt MFT records skipped");
    }
    if stats.deferred_unresolved > 0 {
        tracing::warn!(
            volume = %drive,
            count = stats.deferred_unresolved,
            "attribute-list names unresolved"
        );
    }

    let idx = b.finish();
    stats.elapsed_total_ms = t0.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = peak_working_set();
    Ok((idx, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equivalence gate against the whole-load reference path. Run from an
    /// elevated shell: FMF_ADMIN_TESTS=1 cargo test -- --ignored streaming
    /// The volume is live, so a small drift tolerance is allowed.
    #[test]
    #[ignore]
    fn streaming_scan_matches_reference() {
        if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
            eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
            return;
        }
        let (new_idx, new_stats) = scan_volume("C:").expect("streaming scan");
        let (old_idx, old_stats) = crate::mft::scan_volume_reference("C:").expect("reference");

        let drift = (new_idx.len() as i64 - old_idx.len() as i64).unsigned_abs();
        assert!(
            drift < old_idx.len() as u64 / 500,
            "entry counts diverged: streaming {} vs reference {} (files {}/{} dirs {}/{})",
            new_idx.len(),
            old_idx.len(),
            new_stats.files,
            old_stats.files,
            new_stats.dirs,
            old_stats.dirs,
        );

        // Sampled records must agree on name and size where both saw them.
        let mut checked = 0u64;
        let mut matched = 0u64;
        for sample in (0..old_idx.len() as u32).step_by(997) {
            let old_rec = crate::index::masked(old_idx.frn(sample));
            let (Some(o), Some(n)) = (
                old_idx.entry_by_record(old_rec),
                new_idx.entry_by_record(old_rec),
            ) else {
                continue;
            };
            checked += 1;
            if old_idx.name(o) == new_idx.name(n) && old_idx.size(o) == new_idx.size(n) {
                matched += 1;
            }
        }
        assert!(checked > 100, "sample too small: {checked}");
        assert!(
            matched as f64 / checked as f64 > 0.999,
            "sampled mismatch: {matched}/{checked}"
        );
    }
}
