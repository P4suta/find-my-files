//! Initial full-volume index source: raw $MFT scan via ntfs-reader.
//! Holds the measurement spike (`spike_scan`) and the whole-$MFT reference
//! scanner used as the streaming scanner's equivalence gate.

use std::time::Instant;

use ntfs_reader::api::{NtfsAttributeType, NtfsFileName, NtfsFileNamespace};
use ntfs_reader::errors::NtfsReaderError;
use ntfs_reader::file::NtfsFile;
use ntfs_reader::mft::Mft;
use ntfs_reader::volume::Volume;
use thiserror::Error;

use crate::index::{RawEntry, VolumeIndex, VolumeIndexBuilder};

// The production scanner (and the ScanStats both scanners fill) lives in
// crate::scan; re-exported here so callers keep one import path.
pub use crate::scan::{ScanStats, scan_volume};

#[derive(Debug, Error)]
pub enum MftError {
    #[error("volume scan requires an elevated process (run from an administrator terminal)")]
    NotElevated,
    #[error("ntfs-reader: {0}")]
    Ntfs(#[from] NtfsReaderError),
}

/// Measurements from a full $MFT scan of one volume.
#[derive(Debug, Default)]
pub struct SpikeStats {
    pub volume: String,
    pub elapsed_volume_open_ms: u64,
    /// Time for `Mft::new`: reads the whole $MFT into memory + fixups.
    pub elapsed_mft_load_ms: u64,
    /// Time to walk every in-use record and extract name/size/dates.
    pub elapsed_iterate_ms: u64,
    /// Size of the raw $MFT — the peak-RAM driver of this approach.
    pub mft_bytes: u64,
    pub total_records: u64,
    pub files: u64,
    pub dirs: u64,
    pub reparse_points: u64,
    /// Records where the base record holds no usable $`FILE_NAME` (needs
    /// attribute-list handling in M0).
    pub no_name_in_base_record: u64,
    pub name_utf16_units_total: u64,
    pub max_name_utf16_units: u64,
    /// Sanity check that `reference_number()` carries a sequence value.
    pub frn_sequence_nonzero: u64,
    pub peak_working_set_bytes: u64,
}

impl SpikeStats {
    #[must_use]
    pub fn avg_name_utf16_units(&self) -> f64 {
        let named = (self.files + self.dirs).max(1);
        self.name_utf16_units_total as f64 / named as f64
    }
}

/// Pick the display name like Everything does: prefer Win32 / Win32+DOS
/// namespaces, fall back to POSIX, ignore DOS-only short names. Unlike
/// ntfs-reader's `get_best_file_name`, reparse-point names are kept —
/// junctions and symlinks are indexed as plain entries.
pub(crate) fn pick_name(file: &NtfsFile) -> Option<NtfsFileName> {
    let mut best: Option<NtfsFileName> = None;
    file.attributes(|att| {
        if att.header.type_id != NtfsAttributeType::FileName as u32 {
            return;
        }
        let Some(name) = att.as_name() else { return };
        let ns = name.header.namespace;
        let win32 =
            ns == NtfsFileNamespace::Win32 as u8 || ns == NtfsFileNamespace::Win32AndDos as u8;
        if win32 || (ns == NtfsFileNamespace::Posix as u8 && best.is_none()) {
            best = Some(name);
        }
    });
    best
}

/// Scan one volume's $MFT end to end and report measurements.
/// `drive` is a drive letter spec like `C:`.
///
/// # Errors
///
/// Returns [`MftError::NotElevated`] when the process lacks the privileges to
/// open the raw volume, or [`MftError::Ntfs`] if opening the volume or
/// reading the $MFT fails.
pub fn spike_scan(drive: &str) -> Result<SpikeStats, MftError> {
    let volume_path = format!(r"\\.\{}", drive.trim_end_matches(['\\', '/']));
    let mut stats = SpikeStats {
        volume: drive.to_string(),
        ..Default::default()
    };

    let t0 = Instant::now();
    let volume = Volume::new(&volume_path).map_err(|e| match e {
        NtfsReaderError::ElevationError => MftError::NotElevated,
        other => MftError::Ntfs(other),
    })?;
    stats.elapsed_volume_open_ms = t0.elapsed().as_millis() as u64;

    let t1 = Instant::now();
    let mft = Mft::new(volume)?;
    stats.elapsed_mft_load_ms = t1.elapsed().as_millis() as u64;
    stats.mft_bytes = mft.data.len() as u64;
    stats.total_records = mft.max_record;

    let t2 = Instant::now();
    let mut std_info_seen = 0u64;
    for file in mft.files() {
        let Some(name) = pick_name(&file) else {
            stats.no_name_in_base_record += 1;
            continue;
        };

        let len = name.header.name_length as u64;
        stats.name_utf16_units_total += len;
        stats.max_name_utf16_units = stats.max_name_utf16_units.max(len);

        if file.is_directory() {
            stats.dirs += 1;
        } else {
            stats.files += 1;
        }
        if name.is_reparse_point() {
            stats.reparse_points += 1;
        }
        if file.reference_number() >> 48 != 0 {
            stats.frn_sequence_nonzero += 1;
        }

        // Touch $STANDARD_INFORMATION and $DATA the way the real indexer will,
        // so iteration cost is representative.
        file.attributes(|att| {
            if att.header.type_id == NtfsAttributeType::StandardInformation as u32
                && att.as_standard_info().is_some()
            {
                std_info_seen += 1;
            }
        });
    }
    // Keep the optimizer from dropping the attribute walk.
    std::hint::black_box(std_info_seen);
    stats.elapsed_iterate_ms = t2.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = peak_working_set();

    Ok(stats)
}

/// Full initial scan: read the volume's $MFT and build the in-memory index.
/// `drive` is a drive letter spec like `C:`.
///
/// # Errors
///
/// Returns [`MftError::NotElevated`] when the process lacks the privileges to
/// open the raw volume, or [`MftError::Ntfs`] if opening the volume or
/// reading the $MFT fails.
pub fn scan_volume_reference(drive: &str) -> Result<(VolumeIndex, ScanStats), MftError> {
    use ntfs_reader::api::ROOT_RECORD;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

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
    let t1 = Instant::now();
    let mft = Mft::new(volume)?;
    stats.elapsed_mft_load_ms = t1.elapsed().as_millis() as u64;
    stats.mft_bytes = mft.data.len() as u64;

    let mut b = VolumeIndexBuilder::new(drive, ROOT_RECORD);
    for file in mft.files() {
        // files() yields extension records too (no base_reference filter in
        // ntfs-reader). They are parts of other files; indexing them would
        // duplicate every fragmented file that keeps its $FILE_NAME in an
        // extension record — skip, like the streaming scanner does.
        if { file.header.base_reference } & 0x0000_FFFF_FFFF_FFFF != 0 {
            stats.extension_records += 1;
            continue;
        }
        // Names of heavily fragmented files live in extension records via
        // $ATTRIBUTE_LIST — fall back to ntfs-reader's resolver for those
        // (~4% of records on a real C:).
        let Some(name) = pick_name(&file).or_else(|| file.get_best_file_name(&mft)) else {
            stats.skipped_no_name += 1;
            continue;
        };
        // Copy fields out of the packed structs before borrowing.
        let name_data = name.data;
        let name_len = name.header.name_length as usize;
        let parent_record = name.header.parent_directory_reference;

        let mut size = 0u64;
        let mut mtime = 0i64;
        // Attribute flags in $FILE_NAME are updated lazily by NTFS; the
        // authoritative copy lives in $STANDARD_INFORMATION.
        let mut is_reparse = false;
        let mut is_hidden = false;
        let mut is_system = false;
        file.attributes(|att| {
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

        if file.is_directory() {
            stats.dirs += 1;
        } else {
            stats.files += 1;
        }
        b.push(RawEntry {
            record: file.number,
            parent_record,
            frn: file.reference_number(),
            name_utf16: &name_data[..name_len],
            is_dir: file.is_directory(),
            is_reparse,
            is_hidden,
            is_system,
            size,
            mtime,
        });
    }

    let idx = b.finish();
    stats.elapsed_total_ms = t0.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = peak_working_set();
    Ok((idx, stats))
}

/// Peak working set of the current process, in bytes (0 if the query fails).
#[must_use]
pub fn peak_working_set() -> u64 {
    memory_counters().map_or(0, |c| c.PeakWorkingSetSize as u64)
}

/// Current working set — the steady-state number the RAM gate cares about
/// (the peak includes transient scan buffers).
#[must_use]
pub fn current_working_set() -> u64 {
    memory_counters().map_or(0, |c| c.WorkingSetSize as u64)
}

fn memory_counters() -> Option<windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &raw mut counters,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        );
        (ok != 0).then_some(counters)
    }
}
