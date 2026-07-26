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

use crate::index::{Frn, RawEntry, VolumeIndex, VolumeIndexBuilder};
use crate::usn::MetadataSource;
use crate::usn::apply::LinkSnapshot;

// The production scanner (and the ScanStats both scanners fill) lives in
// crate::scan; re-exported here so callers keep one import path.
pub(crate) use crate::scan::scan_volume_cancellable;
pub use crate::scan::{ScanStats, scan_volume};

/// Failure modes of a raw $MFT volume scan.
#[derive(Debug, Error)]
pub enum MftError {
    /// The owning volume worker requested shutdown. No partial index is
    /// returned or installed.
    #[error("volume scan cancelled")]
    Cancelled,
    /// The process lacks the privileges to open the raw volume (MFT/USN reads
    /// require an elevated process; run from an administrator terminal).
    #[error("volume scan requires an elevated process (run from an administrator terminal)")]
    NotElevated,
    /// An error surfaced by the underlying ntfs-reader (volume open or $MFT read).
    #[error("ntfs-reader: {0}")]
    Ntfs(#[from] NtfsReaderError),
    /// The independent reference scanner could not open or query its live
    /// per-record NTFS metadata source.
    #[error("volume metadata: {0}")]
    Metadata(#[from] crate::usn::UsnError),
    /// Neither the streamed MFT view nor the live exact-FRN fallback could
    /// prove a complete hard-link set. No partial index is published.
    #[error("incomplete metadata for file reference {0}")]
    IncompleteMetadata(u64),
    /// One or more non-empty MFT slots failed signature, fixup, or complete
    /// attribute-chain validation. Publishing would make files disappear.
    #[error("{0} corrupt MFT record(s); refusing to publish a partial index")]
    CorruptRecords(u64),
}

/// Measurements from a full $MFT scan of one volume.
#[derive(Debug, Default)]
pub struct SpikeStats {
    /// Drive letter spec of the scanned volume (e.g. `C:`).
    pub volume: String,
    /// Time to open the raw volume handle, in milliseconds.
    pub elapsed_volume_open_ms: u64,
    /// Time for `Mft::new`: reads the whole $MFT into memory + fixups.
    pub elapsed_mft_load_ms: u64,
    /// Time to walk every in-use record and extract name/size/dates.
    pub elapsed_iterate_ms: u64,
    /// Size of the raw $MFT — the peak-RAM driver of this approach.
    pub mft_bytes: u64,
    /// Total number of $MFT records walked (in-use and free), a count.
    pub total_records: u64,
    /// Searchable file-link rows observed in base records.
    pub files: u64,
    /// Searchable directory-link rows observed in base records.
    pub dirs: u64,
    /// Searchable link rows marked as reparse points (junction/symlink).
    pub reparse_points: u64,
    /// Records where the base record holds no usable `$FILE_NAME`; the
    /// production scanner resolves these through `$ATTRIBUTE_LIST`.
    pub no_name_in_base_record: u64,
    /// Sum of name lengths across all searchable link rows, in UTF-16 code units.
    pub name_utf16_units_total: u64,
    /// Longest single name encountered, in UTF-16 code units.
    pub max_name_utf16_units: u64,
    /// Sanity check that `reference_number()` carries a sequence value.
    pub frn_sequence_nonzero: u64,
    /// Peak working set of the process during the scan, in bytes.
    pub peak_working_set_bytes: u64,
}

impl SpikeStats {
    /// Mean name length across named records, in UTF-16 code units.
    #[must_use]
    pub fn avg_name_utf16_units(&self) -> f64 {
        let named = (self.files + self.dirs).max(1);
        self.name_utf16_units_total as f64 / named as f64
    }
}

pub(crate) const fn is_searchable_namespace(namespace: u8) -> bool {
    namespace == NtfsFileNamespace::Win32 as u8
        || namespace == NtfsFileNamespace::Win32AndDos as u8
        || namespace == NtfsFileNamespace::Posix as u8
}

pub(crate) struct SearchableNames {
    first: Option<NtfsFileName>,
    additional: Vec<NtfsFileName>,
}

impl SearchableNames {
    pub(crate) fn len(&self) -> usize {
        usize::from(self.first.is_some()) + self.additional.len()
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.first.is_none()
    }
}

impl IntoIterator for SearchableNames {
    type Item = NtfsFileName;
    type IntoIter =
        std::iter::Chain<std::option::IntoIter<NtfsFileName>, std::vec::IntoIter<NtfsFileName>>;

    fn into_iter(self) -> Self::IntoIter {
        self.first.into_iter().chain(self.additional)
    }
}

pub(crate) fn collect_searchable_names(file: &NtfsFile<'_>) -> Option<SearchableNames> {
    let mut names = SearchableNames {
        first: None,
        additional: Vec::new(),
    };
    let mut valid = true;
    file.attributes(|attribute| {
        if attribute.header.type_id != NtfsAttributeType::FileName as u32 {
            return;
        }
        let Some(name) = attribute.as_name() else {
            valid = false;
            return;
        };
        if name.header.name_length == 0 {
            valid = false;
            return;
        }
        if is_searchable_namespace(name.header.namespace) {
            if names.first.is_none() {
                names.first = Some(name);
            } else {
                names.additional.push(name);
            }
        } else if name.header.namespace != NtfsFileNamespace::Dos as u8 {
            valid = false;
        }
    });
    valid.then_some(names)
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
        let Some(names) = collect_searchable_names(&file) else {
            stats.no_name_in_base_record += 1;
            continue;
        };
        if names.is_empty() {
            stats.no_name_in_base_record += 1;
            continue;
        }
        let is_dir = file.is_directory();
        for name in names {
            let len = name.header.name_length as u64;
            stats.name_utf16_units_total += len;
            stats.max_name_utf16_units = stats.max_name_utf16_units.max(len);
            if is_dir {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
            if name.is_reparse_point() {
                stats.reparse_points += 1;
            }
        }
        if file.reference_number() >> 48 != 0 {
            stats.frn_sequence_nonzero += 1;
        }

        // Touch `$STANDARD_INFORMATION` and `$DATA` the way the real indexer
        // does, so the measurement covers the same attribute walk.
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
    let metadata = MetadataSource::open_volume(drive)?;
    stats.mft_bytes = mft.data.len() as u64;

    let mut b = VolumeIndexBuilder::new(drive, ROOT_RECORD);
    let mut corrupt_records = 0u64;
    for file in mft.files() {
        // files() yields extension records too (no base_reference filter in
        // ntfs-reader). They are parts of other files; indexing them would
        // duplicate every fragmented file that keeps its $FILE_NAME in an
        // extension record — skip, like the streaming scanner does.
        if { file.header.base_reference } & 0x0000_FFFF_FFFF_FFFF != 0 {
            stats.extension_records += 1;
            continue;
        }
        let frn = file.reference_number();
        let mut links = Vec::new();
        if file
            .get_attribute(NtfsAttributeType::AttributeList)
            .is_some()
        {
            match metadata.links(frn) {
                LinkSnapshot::Present(found) if !found.is_empty() => {
                    links.extend(found.into_iter().map(|link| (link.parent_frn, link.name)));
                }
                LinkSnapshot::Gone => continue,
                LinkSnapshot::Present(_) | LinkSnapshot::Failed => {
                    return Err(MftError::IncompleteMetadata(frn));
                }
            }
        } else {
            let Some(names) = collect_searchable_names(&file) else {
                corrupt_records += 1;
                continue;
            };
            if !names.is_empty() {
                for name in names {
                    let data = name.data;
                    let units = name.header.name_length as usize;
                    links.push((
                        name.header.parent_directory_reference,
                        data[..units].to_vec(),
                    ));
                }
            }
        }
        if links.is_empty() {
            stats.skipped_no_name += 1;
            continue;
        }

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
            } else if att.header.type_id == NtfsAttributeType::Data as u32
                && att.header.name_length == 0
            {
                if att.header.is_non_resident == 0 {
                    if let Some(h) = att.resident_header() {
                        size = h.value_length as u64;
                    }
                } else if let Some(h) = att.nonresident_header() {
                    size = h.data_size;
                }
            }
        });

        let is_dir = file.is_directory();
        for (parent_frn, name) in links {
            if is_dir {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
            b.push(RawEntry {
                parent_frn: Frn(parent_frn),
                frn: Frn(frn),
                name_utf16: &name,
                is_dir,
                is_reparse,
                is_hidden,
                is_system,
                size,
                mtime,
            });
        }
    }

    if corrupt_records > 0 {
        return Err(MftError::CorruptRecords(corrupt_records));
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

/// Current working set — the steady-state number the RAM gate cares about.
///
/// The peak includes transient scan buffers; this is the `Working Set` figure
/// Task Manager / Process Explorer / perfmon report for the process.
#[must_use]
pub fn current_working_set() -> u64 {
    memory_counters().map_or(0, |c| c.WorkingSetSize as u64)
}

/// Private (committed) bytes of the process — `PrivateUsage`.
///
/// This is the `Private Bytes` figure Task Manager / Process Explorer /
/// perfmon report; unlike the working set it is not affected by trimming, so
/// it is the more stable footprint indicator.
#[must_use]
pub fn current_private_bytes() -> u64 {
    memory_counters().map_or(0, |c| c.PrivateUsage as u64)
}

fn memory_counters() -> Option<windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX>
{
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        // GetProcessMemoryInfo fills a PROCESS_MEMORY_COUNTERS_EX when handed a
        // buffer of that size — the EX layout is a strict superset that adds
        // PrivateUsage. The API types the out-param as the base struct, so we
        // pass the EX buffer through a cast.
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
        (ok != 0).then_some(counters)
    }
}
