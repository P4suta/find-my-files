//! Raw volume access: `\\.\C:`-style handles, the NTFS update-sequence
//! fixup, and the logical→physical run map of the $MFT data stream.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdType, GetFileSizeEx, OpenFileById,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS, NTFS_EXTENDED_VOLUME_DATA,
    NTFS_VOLUME_DATA_BUFFER, RETRIEVAL_POINTERS_BUFFER, RETRIEVAL_POINTERS_BUFFER_0,
    STARTING_VCN_INPUT_BUFFER,
};

use crate::ondisk::attribute_list::{StreamRun, decode_extent_runs};
use crate::ondisk::fixup::{apply_fixup, fixup_layout};
use crate::ondisk::ntfs::{
    NtfsAttributeType, NtfsError, NtfsFile, VolumeGeometry, decode_boot_sector,
};
use crate::ondisk::record::attributes_complete;
use crate::volume_label::VolumeLabel;

const BOOT_SECTOR_BYTES: usize = 512;
const RETRIEVAL_OUTPUT_BYTES: usize = 64 << 10;
const RETRIEVAL_EXTENTS_OFFSET: usize = std::mem::offset_of!(RETRIEVAL_POINTERS_BUFFER, Extents);
const RETRIEVAL_EXTENT_BYTES: usize = size_of::<RETRIEVAL_POINTERS_BUFFER_0>();

#[repr(C)]
#[derive(Default)]
struct NtfsVolumeOutput {
    basic: NTFS_VOLUME_DATA_BUFFER,
    extended: NTFS_EXTENDED_VOLUME_DATA,
}

struct ValidatedVolumeGeometry {
    boot: VolumeGeometry,
    mft_valid_data_length: u64,
}

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
    pub(super) sector_size: usize,
    pub(super) root_reference: u64,
    pub(super) data_size: u64,
    pub(super) runmap: RunMap,
    pub(super) cluster_size: u64,
    pub(super) volume_size: u64,
}

fn validate_root_record(file: &NtfsFile<'_>) -> bool {
    if file.number != crate::ondisk::ntfs::ROOT_RECORD
        || !file.is_used()
        || !file.is_directory()
        || file.header.base_reference != 0
        || file.reference_number() >> 48 == 0
    {
        return false;
    }
    let root_reference = file.reference_number();
    let mut standard_information = 0usize;
    let mut file_names = 0usize;
    let mut valid = true;
    file.attributes(|attribute| match attribute.header.type_id {
        type_id if type_id == NtfsAttributeType::StandardInformation as u32 => {
            if attribute.header.name_length != 0
                || attribute.header.flags != 0
                || attribute.header.is_non_resident != 0
                || attribute.as_standard_info().is_none()
            {
                valid = false;
            } else {
                standard_information += 1;
            }
        }
        type_id if type_id == NtfsAttributeType::FileName as u32 => {
            if attribute.header.name_length != 0 || attribute.header.flags != 0 {
                valid = false;
                return;
            }
            let Some(name) = attribute.as_name() else {
                valid = false;
                return;
            };
            if name.header.parent_directory_reference == root_reference {
                file_names += 1;
            } else {
                valid = false;
            }
        }
        _ => {}
    });
    valid && standard_information == 1 && file_names == 1
}

fn read_root_reference(
    reader: &mut std::fs::File,
    runmap: &RunMap,
    record_size: usize,
    sector_size: usize,
) -> Result<u64, NtfsError> {
    let mut record = vec![0u8; record_size];
    let logical = crate::ondisk::ntfs::ROOT_RECORD
        .checked_mul(record_size as u64)
        .ok_or(NtfsError::InvalidData("root record offset overflow"))?;
    runmap.read_exact_logical(reader, logical, &mut record)?;
    if !NtfsFile::is_valid(&record, sector_size)
        || !apply_fixup(&mut record, sector_size)
        || !attributes_complete(&record)
    {
        return Err(NtfsError::InvalidData("invalid NTFS root record"));
    }
    let file = NtfsFile::parse(crate::ondisk::ntfs::ROOT_RECORD, &record, sector_size).ok_or(
        NtfsError::InvalidData("NTFS root record could not be decoded"),
    )?;
    validate_root_record(&file)
        .then_some(file.reference_number())
        .ok_or(NtfsError::InvalidData(
            "record 5 is not the exact in-use NTFS root directory",
        ))
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
    raw_volume_label(volume_path)
        .ok_or_else(|| invalid_data("raw volume path is not canonical \\\\.\\X: form"))?;
    open_shared_read(volume_path)
}

/// Read arbitrary byte ranges from a raw volume handle.
///
/// A volume handle only accepts reads whose offset *and length* are multiples
/// of the logical sector size; anything else fails with
/// `ERROR_INVALID_PARAMETER` (87) before it ever reaches the device.
///
/// Whole `$MFT` records satisfy that by construction, which is why the record
/// readers use the handle directly. Streamed NTFS structures do not: a
/// non-resident `$ATTRIBUTE_LIST` is walked entry by entry, and an entry header
/// is 26 bytes.
///
/// So every such read is widened to the sectors containing it and sliced back
/// down. One sector is buffered, which also collapses the entry-header and
/// entry-body reads of the same entry into a single device read.
pub struct SectorAlignedReader<'a, R> {
    file: &'a mut R,
    sector_size: u64,
    position: u64,
    buf: Vec<u8>,
    /// Volume offset of `buf[0]`; `None` while the buffer holds nothing.
    buf_start: Option<u64>,
}

impl<'a, R: std::io::Read + std::io::Seek> SectorAlignedReader<'a, R> {
    /// `sector_size` must be the volume's logical sector size, and is rounded
    /// up to a sane floor so a bogus value cannot produce a zero-length read.
    pub fn new(file: &'a mut R, sector_size: usize) -> Self {
        let sector_size = (sector_size as u64).max(512);
        Self {
            file,
            sector_size,
            position: 0,
            buf: Vec::new(),
            buf_start: None,
        }
    }

    fn fill(&mut self) -> std::io::Result<()> {
        use std::io::SeekFrom;

        let aligned = self.position - (self.position % self.sector_size);
        let len = usize::try_from(self.sector_size)
            .map_err(|_| std::io::Error::other("sector size exceeds this process address space"))?;
        self.buf.resize(len, 0);
        self.file.seek(SeekFrom::Start(aligned))?;
        self.file.read_exact(&mut self.buf)?;
        self.buf_start = Some(aligned);
        Ok(())
    }
}

impl<R: std::io::Read + std::io::Seek> std::io::Read for SectorAlignedReader<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let covers = self.buf_start.is_some_and(|start| {
            self.position >= start && self.position - start < self.buf.len() as u64
        });
        if !covers {
            self.fill()?;
        }
        let start = self.buf_start.unwrap_or(self.position);
        let offset = usize::try_from(self.position - start).map_err(|_| {
            std::io::Error::other("sector offset exceeds this process address space")
        })?;
        let available = self.buf.len() - offset;
        let take = available.min(out.len());
        out[..take].copy_from_slice(&self.buf[offset..offset + take]);
        self.position += take as u64;
        Ok(take)
    }
}

impl<R: std::io::Read + std::io::Seek> std::io::Seek for SectorAlignedReader<'_, R> {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;

        // Only absolute seeks are meaningful against a volume: the callers
        // address physical offsets they decoded from a run list.
        self.position = match from {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => self
                .position
                .checked_add_signed(delta)
                .ok_or_else(|| std::io::Error::other("seek before the start of the volume"))?,
            SeekFrom::End(_) => {
                return Err(std::io::Error::other(
                    "seeking from the end of a volume is not supported",
                ));
            }
        };
        Ok(self.position)
    }
}

pub(super) fn open_shared_read(path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

fn raw_volume_label(volume_path: &str) -> Option<VolumeLabel> {
    let drive = volume_path.strip_prefix(r"\\.\")?;
    let label = VolumeLabel::parse(drive)?;
    (drive == label.as_str()).then_some(label)
}

fn volume_root_path(volume_path: &str) -> Option<String> {
    raw_volume_label(volume_path).map(VolumeLabel::root_path)
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
) -> Result<(u64, RunMap), NtfsError> {
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
        return Err(NtfsError::InvalidData("invalid unnamed $MFT data extent"));
    }
    let data_size = data_size.ok_or_else(|| {
        NtfsError::MissingMftAttribute("unnamed non-resident Data extent at VCN 0".to_string())
    })?;
    let map = RunMap::from_stream_runs(&stream_runs).ok_or(NtfsError::InvalidData(
        "$MFT data stream contains a sparse extent",
    ))?;
    if !map.is_valid_partial_mft(volume_size) {
        return Err(NtfsError::InvalidData(
            "$MFT data extents overlap or lie outside the volume",
        ));
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

/// Keep the elevation-vs-I/O distinction the caller reports: opening a raw
/// volume without administrator rights is a permission failure, not a disk one.
fn classify_volume_open(error: std::io::Error) -> NtfsError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        NtfsError::Elevation
    } else {
        NtfsError::Io(error)
    }
}

fn query_ntfs_volume_data(
    file: &std::fs::File,
) -> std::io::Result<(NTFS_VOLUME_DATA_BUFFER, NTFS_EXTENDED_VOLUME_DATA)> {
    let mut output = NtfsVolumeOutput::default();
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            raw(file),
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            (&raw mut output).cast(),
            size_of::<NtfsVolumeOutput>() as u32,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::from_raw_os_error(unsafe {
            GetLastError() as i32
        }));
    }
    let required = size_of::<NTFS_VOLUME_DATA_BUFFER>()
        .checked_add(size_of::<NTFS_EXTENDED_VOLUME_DATA>())
        .ok_or_else(|| invalid_data("NTFS volume-data ABI size overflow"))?;
    if usize::try_from(returned)
        .ok()
        .is_none_or(|size| size < required)
        || std::mem::offset_of!(NtfsVolumeOutput, extended) != size_of::<NTFS_VOLUME_DATA_BUFFER>()
        || usize::try_from(output.extended.ByteCount).ok()
            != Some(size_of::<NTFS_EXTENDED_VOLUME_DATA>())
    {
        return Err(invalid_data(
            "FSCTL_GET_NTFS_VOLUME_DATA omitted required extended geometry",
        ));
    }
    Ok((output.basic, output.extended))
}

fn validate_os_geometry(
    boot: VolumeGeometry,
    basic: NTFS_VOLUME_DATA_BUFFER,
    extended: NTFS_EXTENDED_VOLUME_DATA,
) -> Option<u64> {
    if extended.MajorVersion != 3 || !matches!(extended.MinorVersion, 0 | 1) {
        return None;
    }
    let sectors = u64::try_from(basic.NumberSectors).ok()?;
    let clusters = u64::try_from(basic.TotalClusters).ok()?;
    let mft_lcn = u64::try_from(basic.MftStartLcn).ok()?;
    let mft_valid = u64::try_from(basic.MftValidDataLength).ok()?;
    let sector_size = u64::from(basic.BytesPerSector);
    let cluster_size = u64::from(basic.BytesPerCluster);
    let record_size = u64::from(basic.BytesPerFileRecordSegment);
    let physical_sector = u64::from(extended.BytesPerPhysicalSector);
    let os_volume_size = sectors.checked_mul(sector_size)?;
    let cluster_covered = clusters.checked_mul(cluster_size)?;
    if sector_size != boot.sector_size
        || cluster_size != boot.cluster_size
        || record_size != boot.file_record_size
        || os_volume_size != boot.volume_size
        || mft_lcn.checked_mul(cluster_size)? != boot.mft_position
        || physical_sector < sector_size
        || !physical_sector.is_power_of_two()
        || !physical_sector.is_multiple_of(sector_size)
        || cluster_covered > os_volume_size
        || os_volume_size.checked_sub(cluster_covered)? >= cluster_size
        || mft_valid < record_size
        || mft_valid > os_volume_size
        || !mft_valid.is_multiple_of(record_size)
    {
        return None;
    }
    Some(mft_valid)
}

fn read_geometry(file: &mut std::fs::File) -> Result<ValidatedVolumeGeometry, NtfsError> {
    use std::io::{Read, Seek, SeekFrom};

    let mut boot = [0u8; BOOT_SECTOR_BYTES];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut boot)?;
    let boot = decode_boot_sector(&boot)
        .ok_or(NtfsError::InvalidData("invalid NTFS boot-sector geometry"))?;
    let (basic, extended) = query_ntfs_volume_data(file)?;
    let mft_valid_data_length =
        validate_os_geometry(boot, basic, extended).ok_or(NtfsError::InvalidData(
            "boot-sector and FSCTL NTFS geometry disagree or volume version is unsupported",
        ))?;
    Ok(ValidatedVolumeGeometry {
        boot,
        mft_valid_data_length,
    })
}

pub fn volume_geometry(volume_path: &str) -> Result<VolumeGeometry, NtfsError> {
    let mut file = open_raw_volume(volume_path).map_err(classify_volume_open)?;
    read_geometry(&mut file).map(|geometry| geometry.boot)
}

/// Volume geometry + the $MFT data-run map — the bootstrap shared by the
/// full scan and the I/O probe (record 0 → the $MFT's own data runs).
pub(super) fn mft_layout(volume_path: &str) -> Result<MftLayout, NtfsError> {
    use std::io::{Read, Seek, SeekFrom};

    let mut reader = open_raw_volume(volume_path).map_err(classify_volume_open)?;
    let validated = read_geometry(&mut reader)?;
    let volume = validated.boot;
    let record_size = usize::try_from(volume.file_record_size).map_err(|_| {
        NtfsError::InvalidData("$MFT record size exceeds this process address space")
    })?;
    let sector_size = usize::try_from(volume.sector_size).map_err(|_| {
        NtfsError::InvalidData("NTFS sector size exceeds this process address space")
    })?;
    if record_size < sector_size
        || !record_size.is_multiple_of(sector_size)
        || volume.cluster_size == 0
        || volume.volume_size == 0
        || volume
            .mft_position
            .checked_add(volume.file_record_size)
            .is_none_or(|end| end > volume.volume_size)
    {
        return Err(NtfsError::InvalidData("invalid NTFS boot-sector geometry"));
    }
    let mut rec0 = vec![0u8; record_size];
    reader.seek(SeekFrom::Start(volume.mft_position))?;
    reader.read_exact(&mut rec0)?;
    if !NtfsFile::is_valid(&rec0, sector_size) || !apply_fixup(&mut rec0, sector_size) {
        return Err(NtfsError::InvalidData("invalid $MFT record zero"));
    }
    if fixup_layout(&rec0, sector_size).is_none() || !attributes_complete(&rec0) {
        return Err(NtfsError::InvalidData(
            "record 0 has an invalid fixup or attribute layout",
        ));
    }
    let f0 = NtfsFile::parse(0, &rec0, sector_size)
        .ok_or(NtfsError::InvalidData("record 0 could not be decoded"))?;
    let (size, record_zero_map) =
        decode_record_zero_runs(&f0, volume.cluster_size, volume.volume_size)?;
    if size != validated.mft_valid_data_length {
        return Err(NtfsError::InvalidData(
            "$MFT data size disagrees with FSCTL valid-data length",
        ));
    }
    if !valid_mft_size(size, volume.file_record_size, volume.volume_size) {
        return Err(NtfsError::InvalidData("$MFT logical file size is invalid"));
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
        .map_err(NtfsError::from)?
    };
    let root_reference = read_root_reference(&mut reader, &runmap, record_size, sector_size)?;
    Ok(MftLayout {
        record_size,
        sector_size,
        root_reference,
        data_size: size,
        runmap,
        cluster_size: volume.cluster_size,
        volume_size: volume.volume_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the real thing: a Windows volume handle rejects any read
    /// whose offset or length is not a multiple of the logical sector size,
    /// with `ERROR_INVALID_PARAMETER` (87), before the device is touched.
    ///
    /// Every earlier test of this code used a `Cursor`, which accepts any
    /// offset and length — so it validated a world in which the constraint
    /// does not exist. A non-resident `$ATTRIBUTE_LIST` is read entry by entry
    /// and an entry header is 26 bytes, so on a real volume that path failed
    /// 100% of the time and took the whole index with it.
    struct SectorStrictVolume {
        data: Vec<u8>,
        sector_size: usize,
        position: u64,
    }

    impl std::io::Read for SectorStrictVolume {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let sector = self.sector_size as u64;
            if !self.position.is_multiple_of(sector) || !out.len().is_multiple_of(self.sector_size)
            {
                return Err(std::io::Error::from_raw_os_error(87));
            }
            let start = usize::try_from(self.position).expect("fixture offset fits");
            let end = (start + out.len()).min(self.data.len());
            if start >= self.data.len() {
                return Ok(0);
            }
            let len = end - start;
            out[..len].copy_from_slice(&self.data[start..end]);
            self.position += len as u64;
            Ok(len)
        }
    }

    impl std::io::Seek for SectorStrictVolume {
        fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
            self.position = match from {
                std::io::SeekFrom::Start(offset) => offset,
                _ => return Err(std::io::Error::from_raw_os_error(87)),
            };
            Ok(self.position)
        }
    }

    #[test]
    fn sector_aligned_reader_serves_unaligned_ranges_from_a_strict_volume() {
        use std::io::{Read, Seek, SeekFrom};

        let mut volume = SectorStrictVolume {
            data: (0..2048u32).map(|byte| byte as u8).collect(),
            sector_size: 512,
            position: 0,
        };

        // The exact shape that failed on a real C:: a 26-byte entry header at
        // an offset inside a sector, then the entry body, then a read that
        // straddles the sector boundary.
        let mut reader = SectorAlignedReader::new(&mut volume, 512);
        reader.seek(SeekFrom::Start(600)).expect("absolute seek");
        let mut header = [0u8; 26];
        reader
            .read_exact(&mut header)
            .expect("unaligned header read");
        assert_eq!(header[0], 600u32 as u8);
        assert_eq!(header[25], 625u32 as u8);

        let mut straddle = [0u8; 40];
        reader.seek(SeekFrom::Start(1000)).expect("absolute seek");
        reader
            .read_exact(&mut straddle)
            .expect("read across a sector boundary");
        for (index, byte) in straddle.iter().enumerate() {
            assert_eq!(*byte, (1000 + index) as u8, "byte {index}");
        }
    }

    #[test]
    fn a_strict_volume_rejects_the_unaligned_read_the_adapter_exists_to_avoid() {
        use std::io::{Read, Seek, SeekFrom};

        // Proves the fixture models the constraint rather than assuming it:
        // reading the same 26 bytes straight from the handle fails with 87.
        let mut volume = SectorStrictVolume {
            data: vec![0u8; 2048],
            sector_size: 512,
            position: 0,
        };
        volume.seek(SeekFrom::Start(600)).expect("absolute seek");
        let mut header = [0u8; 26];
        let error = volume.read_exact(&mut header).expect_err("must be refused");
        assert_eq!(error.raw_os_error(), Some(87));
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
        assert_eq!(volume_root_path(r"\\.\Z:"), Some("Z:\\".to_string()));
        assert_eq!(volume_root_path(r"\\.\z:"), None);
        assert_eq!(volume_root_path("C:"), None);
        assert_eq!(volume_root_path(r"\\.\C:\"), None);
        assert_eq!(volume_root_path(r"\\.\Volume{not-in-mvp}"), None);
    }

    #[test]
    fn raw_open_rejects_every_noncanonical_path_before_touching_the_os() {
        for invalid in [
            "C:",
            r"\\.\c:",
            r"\\.\C:\",
            r"\\.\PhysicalDrive0",
            r"\\server\share",
        ] {
            assert_eq!(
                open_raw_volume(invalid)
                    .expect_err("noncanonical raw path must fail")
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn os_and_boot_geometry_must_match_and_version_must_be_supported() {
        let boot = VolumeGeometry {
            sector_size: 512,
            cluster_size: 4096,
            volume_size: 1_024_000,
            file_record_size: 1024,
            mft_position: 16_384,
        };
        let mut basic = NTFS_VOLUME_DATA_BUFFER {
            NumberSectors: 2000,
            TotalClusters: 250,
            BytesPerSector: 512,
            BytesPerCluster: 4096,
            BytesPerFileRecordSegment: 1024,
            MftValidDataLength: 102_400,
            MftStartLcn: 4,
            ..Default::default()
        };
        let mut extended = NTFS_EXTENDED_VOLUME_DATA {
            MajorVersion: 3,
            MinorVersion: 1,
            BytesPerPhysicalSector: 4096,
            ..Default::default()
        };
        assert_eq!(validate_os_geometry(boot, basic, extended), Some(102_400));

        extended.MajorVersion = 4;
        assert_eq!(validate_os_geometry(boot, basic, extended), None);
        extended.MajorVersion = 3;
        basic.BytesPerFileRecordSegment = 2048;
        assert_eq!(validate_os_geometry(boot, basic, extended), None);
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
