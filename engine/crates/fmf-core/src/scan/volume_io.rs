//! Raw volume access: `\\.\C:`-style handles, the NTFS update-sequence
//! fixup, and the logical→physical run map of the $MFT data stream.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use ntfs_reader::api::NtfsAttributeType;
use ntfs_reader::errors::NtfsReaderError;
use ntfs_reader::file::NtfsFile;
use ntfs_reader::mft::Mft;
use ntfs_reader::volume::Volume;
use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdType, GetFileSizeEx, OpenFileById,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_RETRIEVAL_POINTERS, RETRIEVAL_POINTERS_BUFFER, RETRIEVAL_POINTERS_BUFFER_0,
    STARTING_VCN_INPUT_BUFFER,
};

use super::attribute_list::{StreamRun, decode_extent_runs};
use super::record::attributes_complete;

const SECTOR: usize = 512;
const RETRIEVAL_OUTPUT_BYTES: usize = 64 << 10;
const RETRIEVAL_EXTENTS_OFFSET: usize = std::mem::offset_of!(RETRIEVAL_POINTERS_BUFFER, Extents);
const RETRIEVAL_EXTENT_BYTES: usize = size_of::<RETRIEVAL_POINTERS_BUFFER_0>();

/// Logical-byte → physical-byte mapping of the $MFT data stream.
#[derive(Clone)]
pub(super) struct RunMap {
    /// (logical start, physical start, length) — all bytes.
    pub(super) runs: Vec<(u64, u64, u64)>,
}

/// Geometry needed by the streaming MFT reader and by non-resident metadata
/// attributes encountered during the scan.
pub(super) struct MftLayout {
    pub(super) record_size: usize,
    pub(super) data_size: u64,
    pub(super) runmap: RunMap,
    pub(super) cluster_size: u64,
    pub(super) volume_size: u64,
}

impl RunMap {
    fn from_stream_runs(runs: &[StreamRun]) -> Option<Self> {
        let mut v = Vec::with_capacity(runs.len());
        for r in runs {
            v.push((r.logical, r.physical?, r.len));
        }
        v.sort_unstable_by_key(|run| run.0);
        Some(Self { runs: v })
    }

    fn is_valid_partial_mft(&self, volume_size: u64) -> bool {
        let mut previous_end = 0u64;
        let mut physical_ranges = Vec::with_capacity(self.runs.len());
        for &(start, physical, len) in &self.runs {
            if len == 0 || start < previous_end {
                return false;
            }
            let Some(end) = start.checked_add(len) else {
                return false;
            };
            let Some(physical_end) = physical.checked_add(len) else {
                return false;
            };
            if physical_end > volume_size {
                return false;
            }
            physical_ranges.push((physical, physical_end));
            previous_end = end;
        }
        physical_ranges.sort_unstable();
        if physical_ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return false;
        }
        self.runs.first().is_some_and(|run| run.0 == 0)
    }

    fn is_complete_mft(&self, data_size: u64, volume_size: u64) -> bool {
        if data_size == 0 || volume_size == 0 || !self.is_valid_partial_mft(volume_size) {
            return false;
        }
        let mut logical = 0u64;
        for &(start, physical, len) in &self.runs {
            if start != logical || len == 0 {
                return false;
            }
            let Some(next_logical) = logical.checked_add(len) else {
                return false;
            };
            if physical
                .checked_add(len)
                .is_none_or(|end| end > volume_size)
            {
                return false;
            }
            logical = next_logical;
        }
        logical >= data_size
    }

    fn mapping_matches(&self, logical: u64, physical: u64, len: u64) -> bool {
        let Some(end) = logical.checked_add(len) else {
            return false;
        };
        let mut cursor = logical;
        for &(run_logical, run_physical, run_len) in &self.runs {
            let Some(run_end) = run_logical.checked_add(run_len) else {
                return false;
            };
            if cursor >= end {
                return true;
            }
            if run_end <= cursor {
                continue;
            }
            if run_logical > cursor {
                return false;
            }
            let Some(expected) = physical.checked_add(cursor - logical) else {
                return false;
            };
            let Some(actual) = run_physical.checked_add(cursor - run_logical) else {
                return false;
            };
            if actual != expected {
                return false;
            }
            cursor = run_end.min(end);
        }
        cursor == end
    }

    fn contains_mappings(&self, other: &Self) -> bool {
        other
            .runs
            .iter()
            .all(|&(logical, physical, len)| self.mapping_matches(logical, physical, len))
    }

    pub(super) fn data_spans(&self, logical: u64, len: usize) -> Option<Vec<ReadSpan>> {
        let mut previous_end = 0u64;
        for &(start, physical, run_len) in &self.runs {
            if run_len == 0 || start < previous_end || physical.checked_add(run_len).is_none() {
                return None;
            }
            previous_end = start.checked_add(run_len)?;
        }
        let len_u64 = u64::try_from(len).ok()?;
        let end = logical.checked_add(len_u64)?;
        let mut cursor = logical;
        let mut spans = Vec::new();
        while cursor < end {
            if let Some(&(run_logical, physical, run_len)) = self.runs.iter().find(|run| {
                cursor >= run.0
                    && run
                        .0
                        .checked_add(run.2)
                        .is_some_and(|run_end| cursor < run_end)
            }) {
                let delta = cursor.checked_sub(run_logical)?;
                let available = run_len.checked_sub(delta)?;
                let take = available.min(end - cursor);
                spans.push(ReadSpan {
                    output_offset: usize::try_from(cursor - logical).ok()?,
                    physical: physical.checked_add(delta)?,
                    len: usize::try_from(take).ok()?,
                });
                cursor = cursor.checked_add(take)?;
                continue;
            }

            // A gap is a sparse run. The destination starts zero-filled, so
            // only advance to the next physical extent (or the request end).
            let next = self
                .runs
                .iter()
                .filter_map(|(start, _, _)| (*start > cursor).then_some(*start))
                .min()
                .unwrap_or(end)
                .min(end);
            if next <= cursor {
                return None;
            }
            cursor = next;
        }
        Some(spans)
    }

    pub(super) fn read_exact_logical(
        &self,
        file: &mut std::fs::File,
        logical: u64,
        output: &mut [u8],
    ) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};

        output.fill(0);
        let spans = self.data_spans(logical, output.len()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid NTFS run map")
        })?;
        for span in spans {
            let end = span
                .output_offset
                .checked_add(span.len)
                .ok_or_else(|| std::io::Error::other("run span overflow"))?;
            file.seek(SeekFrom::Start(span.physical))?;
            file.read_exact(&mut output[span.output_offset..end])?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReadSpan {
    pub(super) output_offset: usize,
    pub(super) physical: u64,
    pub(super) len: usize,
}

pub fn open_raw_volume(volume_path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(volume_path)
}

fn volume_root_path(volume_path: &str) -> Option<String> {
    let drive = volume_path.strip_prefix(r"\\.\").filter(|drive| {
        let bytes = drive.as_bytes();
        bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
    })?;
    Some(format!("{drive}\\"))
}

fn open_volume_hint(volume_path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    let root = volume_root_path(volume_path)
        .ok_or_else(|| invalid_data("raw volume path is not a canonical drive-letter volume"))?;
    // OpenFileById documents hVolumeHint as a file-system handle on the
    // target volume. A root-directory handle satisfies that contract;
    // `\\.\C:` is instead a DASD handle, for which CreateFile explicitly
    // warns that unrelated file APIs can behave differently.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(root)
}

/// Apply the NTFS update sequence array in place. Returns false when the
/// sector check bytes don't match (torn/corrupt record).
pub fn apply_fixup(data: &mut [u8]) -> bool {
    let Some((uso, usl)) = fixup_layout(data) else {
        return false;
    };
    let usn = [data[uso], data[uso + 1]];
    let fixups: Vec<[u8; 2]> = (1..usl)
        .map(|i| {
            let usa_off = uso + i * 2;
            [data[usa_off], data[usa_off + 1]]
        })
        .collect();

    // Validate every sector before mutating any of them. Besides keeping a
    // failed record untouched, copying the USA first prevents a malicious USA
    // range that overlaps a sector tail from changing a later replacement.
    for sector in 1..usl {
        let sector_off = sector * SECTOR - 2;
        if data[sector_off..sector_off + 2] != usn {
            return false;
        }
    }
    for (sector, fixup) in fixups.into_iter().enumerate() {
        let sector_off = (sector + 1) * SECTOR - 2;
        data[sector_off..sector_off + 2].copy_from_slice(&fixup);
    }
    true
}

fn fixup_layout(data: &[u8]) -> Option<(usize, usize)> {
    if data.len() < SECTOR || !data.len().is_multiple_of(SECTOR) {
        return None;
    }
    let uso = u16::from_le_bytes([data[4], data[5]]) as usize;
    let usl = u16::from_le_bytes([data[6], data[7]]) as usize;
    let expected_usl = data.len().checked_div(SECTOR)?.checked_add(1)?;
    let usa_bytes = usl.checked_mul(2)?;
    let usa_end = uso.checked_add(usa_bytes)?;
    let attributes_offset = u16::from_le_bytes([data[20], data[21]]) as usize;
    if uso < 8
        || !uso.is_multiple_of(2)
        || usl != expected_usl
        || usa_end > data.len()
        || usa_end > attributes_offset
        || attributes_offset >= data.len()
    {
        return None;
    }
    Some((uso, usl))
}

fn le_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn le_i64(data: &[u8], offset: usize) -> Option<i64> {
    let bytes: [u8; 8] = data.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(i64::from_le_bytes(bytes))
}

fn checked_existing_coverage(
    runs: &[(u64, u64, u64)],
    cluster_size: u64,
    volume_size: u64,
) -> Option<u64> {
    let mut covered = 0u64;
    for &(logical, physical, len) in runs {
        if logical != covered
            || len == 0
            || !logical.is_multiple_of(cluster_size)
            || !physical.is_multiple_of(cluster_size)
            || !len.is_multiple_of(cluster_size)
            || physical.checked_add(len)? > volume_size
        {
            return None;
        }
        covered = covered.checked_add(len)?;
    }
    Some(covered)
}

/// Merge one `FSCTL_GET_RETRIEVAL_POINTERS` page into a contiguous byte run
/// map. Windows may round the returned `StartingVcn` down to the beginning of
/// the containing extent. Such overlap is accepted only if every repeated
/// logical byte maps to the same physical byte as the preceding page.
fn append_retrieval_page(
    data: &[u8],
    requested_vcn: u64,
    cluster_size: u64,
    volume_size: u64,
    runs: &mut Vec<(u64, u64, u64)>,
) -> Option<u64> {
    if cluster_size == 0 || volume_size == 0 || data.len() < RETRIEVAL_EXTENTS_OFFSET {
        return None;
    }
    let requested_logical = requested_vcn.checked_mul(cluster_size)?;
    let mut covered = checked_existing_coverage(runs, cluster_size, volume_size)?;
    if covered != requested_logical {
        return None;
    }

    let extent_count = usize::try_from(le_u32(data, 0)?).ok()?;
    if extent_count == 0 {
        return None;
    }
    let extents_bytes = extent_count.checked_mul(RETRIEVAL_EXTENT_BYTES)?;
    if RETRIEVAL_EXTENTS_OFFSET.checked_add(extents_bytes)? > data.len() {
        return None;
    }

    let starting_vcn = u64::try_from(le_i64(
        data,
        std::mem::offset_of!(RETRIEVAL_POINTERS_BUFFER, StartingVcn),
    )?)
    .ok()?;
    if starting_vcn > requested_vcn {
        return None;
    }

    let existing = RunMap { runs: runs.clone() };
    let mut merged = runs.clone();
    let mut current_vcn = starting_vcn;
    for i in 0..extent_count {
        let extent_offset =
            RETRIEVAL_EXTENTS_OFFSET.checked_add(i.checked_mul(RETRIEVAL_EXTENT_BYTES)?)?;
        let next_vcn = u64::try_from(le_i64(data, extent_offset)?).ok()?;
        let lcn = u64::try_from(le_i64(data, extent_offset.checked_add(8)?)?).ok()?;
        if next_vcn <= current_vcn {
            return None;
        }
        let logical = current_vcn.checked_mul(cluster_size)?;
        let end = next_vcn.checked_mul(cluster_size)?;
        let physical = lcn.checked_mul(cluster_size)?;
        let len = end.checked_sub(logical)?;
        if physical.checked_add(len)? > volume_size {
            return None;
        }

        if logical < covered {
            let overlap_end = end.min(covered);
            if !existing.mapping_matches(logical, physical, overlap_end - logical) {
                return None;
            }
        }
        if end > covered {
            if logical > covered {
                return None;
            }
            let trim = covered - logical;
            merged.push((covered, physical.checked_add(trim)?, end - covered));
            covered = end;
        }
        current_vcn = next_vcn;
    }

    let final_logical = current_vcn.checked_mul(cluster_size)?;
    if final_logical != covered || final_logical <= requested_logical {
        return None;
    }
    *runs = merged;
    Some(current_vcn)
}

fn raw(handle: &impl AsRawHandle) -> HANDLE {
    handle.as_raw_handle() as HANDLE
}

fn invalid_data(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn open_file_by_reference(
    volume: &impl AsRawHandle,
    full_reference: u64,
) -> std::io::Result<OwnedHandle> {
    let mut descriptor = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        ..Default::default()
    };
    descriptor.Anonymous.FileId = full_reference as i64;
    let handle = unsafe {
        OpenFileById(
            raw(volume),
            &raw const descriptor,
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::from_raw_os_error(unsafe {
            GetLastError() as i32
        }));
    }
    // SAFETY: `OpenFileById` returned a fresh owned handle and the invalid
    // sentinel was rejected above.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
}

fn file_size(handle: &impl AsRawHandle) -> std::io::Result<u64> {
    let mut size = 0i64;
    if unsafe { GetFileSizeEx(raw(handle), &raw mut size) } == 0 {
        return Err(std::io::Error::from_raw_os_error(unsafe {
            GetLastError() as i32
        }));
    }
    u64::try_from(size).map_err(|_| invalid_data("$MFT has a negative logical size"))
}

fn query_retrieval_runmap(
    handle: &impl AsRawHandle,
    cluster_size: u64,
    data_size: u64,
    volume_size: u64,
) -> std::io::Result<RunMap> {
    let mut runs = Vec::new();
    let mut requested_vcn = 0u64;
    let mut output = vec![0u8; RETRIEVAL_OUTPUT_BYTES];
    let output_len = u32::try_from(output.len())
        .map_err(|_| invalid_data("retrieval-pointer output buffer is too large"))?;

    loop {
        let input = STARTING_VCN_INPUT_BUFFER {
            StartingVcn: i64::try_from(requested_vcn)
                .map_err(|_| invalid_data("$MFT VCN does not fit the Windows ABI"))?,
        };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                raw(handle),
                FSCTL_GET_RETRIEVAL_POINTERS,
                (&raw const input).cast(),
                size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                output.as_mut_ptr().cast(),
                output_len,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        let error = if ok == 0 {
            // `ERROR_MORE_DATA` is the documented pagination signal and the
            // returned prefix is valid. Capture the code immediately.
            unsafe { GetLastError() }
        } else {
            0
        };
        if ok == 0 && error != ERROR_MORE_DATA {
            return Err(std::io::Error::from_raw_os_error(error as i32));
        }
        let returned = usize::try_from(returned)
            .ok()
            .filter(|&len| len <= output.len())
            .ok_or_else(|| invalid_data("retrieval-pointer byte count exceeds its buffer"))?;
        let next_vcn = append_retrieval_page(
            &output[..returned],
            requested_vcn,
            cluster_size,
            volume_size,
            &mut runs,
        )
        .ok_or_else(|| invalid_data("malformed or inconsistent $MFT retrieval pointers"))?;
        requested_vcn = next_vcn;

        if requested_vcn
            .checked_mul(cluster_size)
            .is_some_and(|covered| covered >= data_size)
        {
            let map = RunMap { runs };
            return map
                .is_complete_mft(data_size, volume_size)
                .then_some(map)
                .ok_or_else(|| invalid_data("$MFT retrieval pointers do not cover the stream"));
        }
        if ok != 0 {
            return Err(invalid_data(
                "$MFT retrieval pointers ended before the logical file size",
            ));
        }
    }
}

const fn valid_mft_size(data_size: u64, record_size: u64, volume_size: u64) -> bool {
    data_size >= record_size
        && data_size <= volume_size
        && record_size != 0
        && data_size.is_multiple_of(record_size)
}

fn decode_record_zero_runs(
    file: &NtfsFile<'_>,
    cluster_size: u64,
    volume_size: u64,
) -> Result<(u64, RunMap), NtfsReaderError> {
    let mut data_size = None;
    let mut stream_runs = Vec::new();
    let mut invalid = false;
    file.attributes(|attribute| {
        if attribute.header.type_id != NtfsAttributeType::Data as u32
            || attribute.header.name_length != 0
        {
            return;
        }
        if attribute.header.is_non_resident != 1 || attribute.header.flags != 0 {
            invalid = true;
            return;
        }
        let Some(header) = attribute.nonresident_header() else {
            invalid = true;
            return;
        };
        let Ok(lowest_vcn) = u64::try_from(header.lowest_vcn) else {
            invalid = true;
            return;
        };
        let Some((extent_size, runs)) = decode_extent_runs(attribute, cluster_size, volume_size)
        else {
            invalid = true;
            return;
        };
        if lowest_vcn == 0 && data_size.replace(extent_size).is_some() {
            invalid = true;
            return;
        }
        stream_runs.extend(runs);
    });
    if invalid {
        return Err(NtfsReaderError::InvalidDataRun {
            details: "invalid unnamed $MFT data extent",
        });
    }
    let data_size = data_size.ok_or_else(|| {
        NtfsReaderError::MissingMftAttribute(
            "unnamed non-resident Data extent at VCN 0".to_string(),
        )
    })?;
    let map = RunMap::from_stream_runs(&stream_runs).ok_or(NtfsReaderError::InvalidDataRun {
        details: "$MFT data stream contains a sparse extent",
    })?;
    if !map.is_valid_partial_mft(volume_size) {
        return Err(NtfsReaderError::InvalidDataRun {
            details: "$MFT data extents overlap or lie outside the volume",
        });
    }
    Ok((data_size, map))
}

fn retrieval_fallback(
    volume_path: &str,
    full_reference: u64,
    header_size: u64,
    record_size: u64,
    cluster_size: u64,
    volume_size: u64,
    record_zero_map: &RunMap,
) -> std::io::Result<RunMap> {
    let volume_hint = open_volume_hint(volume_path)?;
    let mft = open_file_by_reference(&volume_hint, full_reference)?;
    let authoritative_size = file_size(&mft)?;
    if authoritative_size != header_size {
        return Err(invalid_data(
            "$MFT attribute size disagrees with the exact file handle",
        ));
    }
    if !valid_mft_size(authoritative_size, record_size, volume_size) {
        return Err(invalid_data("$MFT logical file size is invalid"));
    }
    let map = query_retrieval_runmap(&mft, cluster_size, authoritative_size, volume_size)?;
    if !map.contains_mappings(record_zero_map) {
        return Err(invalid_data(
            "$MFT retrieval pointers disagree with record 0 data extents",
        ));
    }
    Ok(map)
}

/// Volume geometry + the $MFT data-run map — the bootstrap shared by the
/// full scan and the I/O probe (record 0 → the $MFT's own data runs).
pub(super) fn mft_layout(volume_path: &str) -> Result<MftLayout, NtfsReaderError> {
    let volume = Volume::new(volume_path)?;
    let record_size =
        usize::try_from(volume.file_record_size).map_err(|_| NtfsReaderError::InvalidDataRun {
            details: "$MFT record size exceeds this process address space",
        })?;
    if record_size < SECTOR
        || !record_size.is_multiple_of(SECTOR)
        || volume.cluster_size == 0
        || volume.volume_size == 0
        || volume
            .mft_position
            .checked_add(volume.file_record_size)
            .is_none_or(|end| end > volume.volume_size)
    {
        return Err(NtfsReaderError::InvalidDataRun {
            details: "invalid NTFS boot-sector geometry",
        });
    }
    let mut reader = ntfs_reader::aligned_reader::open_volume(std::path::Path::new(volume_path))
        .map_err(NtfsReaderError::from)?;
    let rec0 = Mft::get_record_fs(&mut reader, volume.file_record_size, volume.mft_position)?;
    if fixup_layout(&rec0).is_none() || !attributes_complete(&rec0) {
        return Err(NtfsReaderError::InvalidDataRun {
            details: "record 0 has an invalid fixup or attribute layout",
        });
    }
    let f0 = NtfsFile::new(0, &rec0);
    let (size, record_zero_map) =
        decode_record_zero_runs(&f0, volume.cluster_size, volume.volume_size)?;
    if !valid_mft_size(size, volume.file_record_size, volume.volume_size) {
        return Err(NtfsReaderError::InvalidDataRun {
            details: "$MFT logical file size is invalid",
        });
    }
    let runmap = if record_zero_map.is_complete_mft(size, volume.volume_size) {
        record_zero_map
    } else {
        retrieval_fallback(
            volume_path,
            f0.reference_number(),
            size,
            volume.file_record_size,
            volume.cluster_size,
            volume.volume_size,
            &record_zero_map,
        )
        .map_err(NtfsReaderError::from)?
    };
    Ok(MftLayout {
        record_size,
        data_size: size,
        runmap,
        cluster_size: volume.cluster_size,
        volume_size: volume.volume_size,
    })
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
        let attributes_offset = (uso + usize::from(usl) * 2).next_multiple_of(8);
        r[4..6].copy_from_slice(&(uso as u16).to_le_bytes());
        r[6..8].copy_from_slice(&usl.to_le_bytes());
        r[20..22].copy_from_slice(&(attributes_offset as u16).to_le_bytes());
        r[uso..uso + 2].copy_from_slice(&usn.to_le_bytes());
        for (i, f) in fixups.iter().enumerate() {
            let off = uso + (i + 1) * 2;
            r[off..off + 2].copy_from_slice(&f.to_le_bytes());
            let tail = (i + 1) * SECTOR - 2;
            r[tail..tail + 2].copy_from_slice(&usn.to_le_bytes());
        }
        r
    }

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_i64(data: &mut [u8], offset: usize, value: i64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn retrieval_page(starting_vcn: i64, extents: &[(i64, i64)]) -> Vec<u8> {
        let mut data = vec![0u8; RETRIEVAL_EXTENTS_OFFSET + extents.len() * RETRIEVAL_EXTENT_BYTES];
        put_u32(&mut data, 0, extents.len() as u32);
        put_i64(
            &mut data,
            std::mem::offset_of!(RETRIEVAL_POINTERS_BUFFER, StartingVcn),
            starting_vcn,
        );
        for (i, &(next_vcn, lcn)) in extents.iter().enumerate() {
            let offset = RETRIEVAL_EXTENTS_OFFSET + i * RETRIEVAL_EXTENT_BYTES;
            put_i64(&mut data, offset, next_vcn);
            put_i64(&mut data, offset + 8, lcn);
        }
        data
    }

    #[test]
    fn volume_hint_is_a_canonical_file_system_root_not_a_dasd_handle() {
        assert_eq!(volume_root_path(r"\\.\C:"), Some("C:\\".to_string()));
        assert_eq!(volume_root_path(r"\\.\z:"), Some("z:\\".to_string()));
        assert_eq!(volume_root_path("C:"), None);
        assert_eq!(volume_root_path(r"\\.\C:\"), None);
        assert_eq!(volume_root_path(r"\\.\Volume{not-in-mvp}"), None);
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
    fn rejects_a_usa_that_does_not_cover_every_sector_exactly() {
        let mut too_few = record_with_usa(1024, 48, 0x0001, &[0xAAAA]);
        assert!(!apply_fixup(&mut too_few));

        let mut too_many = vec![0u8; 1024];
        too_many[4..6].copy_from_slice(&48u16.to_le_bytes());
        too_many[6..8].copy_from_slice(&4u16.to_le_bytes());
        assert!(!apply_fixup(&mut too_many));
    }

    #[test]
    fn rejects_a_usa_that_runs_past_the_buffer() {
        let mut r = vec![0u8; 1024];
        r[4..6].copy_from_slice(&1020u16.to_le_bytes()); // uso near the end
        r[6..8].copy_from_slice(&8u16.to_le_bytes()); // uso + usl*2 > len
        assert!(!apply_fixup(&mut r));
    }

    #[test]
    fn rejects_a_misaligned_or_header_overlapping_usa() {
        let mut overlaps_header = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        overlaps_header[4..6].copy_from_slice(&6u16.to_le_bytes());
        assert!(!apply_fixup(&mut overlaps_header));

        let mut misaligned = record_with_usa(1024, 49, 0x0001, &[0xAAAA, 0xBBBB]);
        assert!(!apply_fixup(&mut misaligned));
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

    #[test]
    fn mft_runmap_must_be_contiguous_complete_and_inside_the_volume() {
        let valid = RunMap {
            runs: vec![(0, 4096, 1024), (1024, 8192, 1024)],
        };
        assert!(valid.is_complete_mft(1536, 16_384));

        let sparse = RunMap {
            runs: vec![(0, 4096, 1024), (2048, 8192, 1024)],
        };
        assert!(!sparse.is_complete_mft(2048, 16_384));

        let short = RunMap {
            runs: vec![(0, 4096, 1024)],
        };
        assert!(!short.is_complete_mft(2048, 16_384));

        let outside = RunMap {
            runs: vec![(0, 16_000, 1024)],
        };
        assert!(!outside.is_complete_mft(1024, 16_384));

        let aliased_physical_ranges = RunMap {
            runs: vec![(0, 4096, 1024), (1024, 4608, 1024)],
        };
        assert!(!aliased_physical_ranges.is_complete_mft(2048, 16_384));
    }

    #[test]
    fn retrieval_pages_accept_verified_round_down_and_cross_every_run_boundary() {
        let mut runs = Vec::new();
        let first = retrieval_page(0, &[(2, 10), (4, 20)]);
        assert_eq!(append_retrieval_page(&first, 0, 4, 200, &mut runs), Some(4));
        assert_eq!(runs, vec![(0, 40, 8), (8, 80, 8)]);

        // The second call requests VCN 4, but Windows may round StartingVcn
        // down to the VCN-2 extent. Its repeated mapping must agree exactly.
        let second = retrieval_page(2, &[(4, 20), (6, 30), (8, 40)]);
        assert_eq!(
            append_retrieval_page(&second, 4, 4, 200, &mut runs),
            Some(8)
        );
        assert_eq!(
            runs,
            vec![(0, 40, 8), (8, 80, 8), (16, 120, 8), (24, 160, 8)]
        );

        let map = RunMap { runs };
        assert!(map.is_complete_mft(28, 200));
        assert_eq!(
            map.data_spans(6, 22),
            Some(vec![
                ReadSpan {
                    output_offset: 0,
                    physical: 46,
                    len: 2,
                },
                ReadSpan {
                    output_offset: 2,
                    physical: 80,
                    len: 8,
                },
                ReadSpan {
                    output_offset: 10,
                    physical: 120,
                    len: 8,
                },
                ReadSpan {
                    output_offset: 18,
                    physical: 160,
                    len: 4,
                },
            ])
        );
    }

    #[test]
    fn retrieval_page_rejects_malformed_sparse_and_out_of_volume_mappings() {
        let mut runs = Vec::new();
        assert!(append_retrieval_page(&[], 0, 4, 200, &mut runs).is_none());
        assert!(append_retrieval_page(&retrieval_page(0, &[]), 0, 4, 200, &mut runs).is_none());

        let complete = retrieval_page(0, &[(2, 10)]);
        assert!(
            append_retrieval_page(&complete[..complete.len() - 1], 0, 4, 200, &mut runs).is_none()
        );
        assert!(
            append_retrieval_page(&retrieval_page(1, &[(2, 10)]), 0, 4, 200, &mut runs).is_none()
        );
        assert!(
            append_retrieval_page(&retrieval_page(0, &[(0, 10)]), 0, 4, 200, &mut runs).is_none()
        );
        assert!(
            append_retrieval_page(&retrieval_page(0, &[(1, -1)]), 0, 4, 200, &mut runs).is_none()
        );
        assert!(
            append_retrieval_page(&retrieval_page(0, &[(2, 49)]), 0, 4, 200, &mut runs).is_none()
        );
        assert!(
            append_retrieval_page(
                &retrieval_page(0, &[(i64::MAX, 1)]),
                0,
                u64::MAX,
                u64::MAX,
                &mut runs
            )
            .is_none()
        );
        assert!(runs.is_empty(), "a rejected page must be atomic");
    }

    #[test]
    fn retrieval_page_rejects_a_changed_overlap_mapping_atomically() {
        let mut runs = Vec::new();
        let first = retrieval_page(0, &[(2, 10), (4, 20)]);
        assert_eq!(append_retrieval_page(&first, 0, 4, 200, &mut runs), Some(4));
        let before = runs.clone();

        let changed = retrieval_page(2, &[(4, 21), (6, 30)]);
        assert!(append_retrieval_page(&changed, 4, 4, 200, &mut runs).is_none());
        assert_eq!(runs, before);
    }
}
