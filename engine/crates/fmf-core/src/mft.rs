//! Initial full-volume index source and process-memory measurements.
//!
//! The production scanner lives in [`crate::scan`].  Shared name-selection
//! policy stays here so the initial scan and live-USN reconciliation apply the
//! exact same namespace rules.

use thiserror::Error;

use crate::ondisk::ntfs::{
    NtfsAttributeType, NtfsError, NtfsFile, NtfsFileName, NtfsFileNamespace,
};

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
    /// A checked boot-sector, raw-volume, record, or attribute error.
    #[error("NTFS scan: {0}")]
    Ntfs(String),
    /// The live exact-record metadata source could not be opened or queried.
    #[error("volume metadata: {0}")]
    Metadata(#[from] crate::usn::UsnError),
    /// Neither the streamed MFT view nor the live exact-FRN fallback could
    /// prove a complete hard-link set. No partial index is published.
    #[error("incomplete metadata: {0}")]
    IncompleteMetadata(IncompleteObject),
    /// One or more non-empty MFT slots failed signature, fixup, or complete
    /// attribute-chain validation. Publishing would make files disappear.
    #[error("{0} corrupt MFT record(s); refusing to publish a partial index")]
    CorruptRecords(u64),
}

/// Why one object could not be completed during the deferred
/// $`ATTRIBUTE_LIST` name-resolution pass.
///
/// Each variant is a different operator response — a bad sector, a volume
/// that changed under the scan, and a record grammar this parser cannot read
/// are not the same incident. Collapsing them into a bare file reference
/// (which is what the number alone amounts to) throws away the only part of
/// the report that says what to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncompleteCause {
    /// The base record could not be read back from the raw volume, and the
    /// live source could not prove the object gone either. Typically an I/O
    /// error or a record torn by a concurrent write.
    RecordUnreadable,
    /// The record bytes failed signature, fixup, or attribute-chain
    /// validation — this is a grammar failure, not an I/O failure.
    MalformedRecord,
    /// The record's own reference number disagreed with the one requested:
    /// the slot was recycled while the scan was streaming.
    ReferenceMismatch,
    /// The record parsed, but carried no usable $`STANDARD_INFORMATION` /
    /// $`FILE_NAME` attribute set.
    AttributesMissing,
    /// A resolved link could not be added to the batch (over-long name, or a
    /// batch bound reached) — the object's path set would be short one entry.
    LinkRejected,
    /// $`ATTRIBUTE_LIST` resolution found no names and the live link query
    /// returned no authoritative set, so the complete path set is unknown.
    LinkSetUnavailable,
}

impl std::fmt::Display for IncompleteCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RecordUnreadable => "the base record could not be read from the volume",
            Self::MalformedRecord => "the record failed signature/fixup/attribute validation",
            Self::ReferenceMismatch => "the record belongs to a different object generation",
            Self::AttributesMissing => "the record carries no usable attribute set",
            Self::LinkRejected => "a resolved link did not fit the batch",
            Self::LinkSetUnavailable => "no authoritative hard-link set is available",
        })
    }
}

/// One object the deferred pass gave up on, with everything known about why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncompleteObject {
    /// Full NTFS file reference of the object that could not be completed.
    pub reference: u64,
    /// What specifically failed.
    pub cause: IncompleteCause,
    /// Targeted volume record/stream reads that failed while the same worker
    /// chunk ran. Counted whether or not the chunk went on to give up, so the
    /// tally is never lost precisely when it explains the failure.
    pub name_read_failures: u64,
}

impl std::fmt::Display for IncompleteObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "file reference {}: {} ({} failed volume read(s) in the same chunk)",
            self.reference, self.cause, self.name_read_failures
        )
    }
}

impl From<NtfsError> for MftError {
    fn from(error: NtfsError) -> Self {
        match error {
            NtfsError::Elevation => Self::NotElevated,
            other => Self::Ntfs(other.to_string()),
        }
    }
}

pub(crate) const fn is_searchable_namespace(namespace: u8) -> bool {
    namespace == NtfsFileNamespace::Win32 as u8
        || namespace == NtfsFileNamespace::Win32AndDos as u8
        || namespace == NtfsFileNamespace::Posix as u8
}

pub(crate) struct SearchableNames<'a> {
    first: Option<NtfsFileName<'a>>,
    additional: Vec<NtfsFileName<'a>>,
}

impl SearchableNames<'_> {
    pub(crate) fn len(&self) -> usize {
        usize::from(self.first.is_some()) + self.additional.len()
    }
}

impl<'a> IntoIterator for SearchableNames<'a> {
    type Item = NtfsFileName<'a>;
    type IntoIter = std::iter::Chain<
        std::option::IntoIter<NtfsFileName<'a>>,
        std::vec::IntoIter<NtfsFileName<'a>>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.first.into_iter().chain(self.additional)
    }
}

pub(crate) fn collect_searchable_names<'a>(file: &NtfsFile<'a>) -> Option<SearchableNames<'a>> {
    let mut names = SearchableNames {
        first: None,
        additional: Vec::new(),
    };
    let mut valid = true;
    file.attributes(|attribute| {
        if attribute.header.type_id != NtfsAttributeType::FileName as u32 {
            return;
        }
        if attribute.header.name_length != 0 || attribute.header.flags != 0 {
            valid = false;
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
