//! Live USN-journal session: volume handle, FSCTL wrappers, blocking reads
//! and the per-file metadata fetcher.
//!
//! This is the only OS-facing part of the `usn` module — everything above it
//! works on parsed records.

use std::collections::hash_map::Entry;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use thiserror::Error;
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED,
    ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    OPEN_EXISTING, OpenFileById,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_GET_NTFS_FILE_RECORD,
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, NTFS_FILE_RECORD_INPUT_BUFFER,
    NTFS_FILE_RECORD_OUTPUT_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};

use super::apply::{LinkInfo, LinkSnapshot};
use super::records::{UsnRecord, parse_buffer};
use crate::mft::is_searchable_namespace;
use crate::ondisk::attribute_list::{
    ListEntry, StreamRun, close_extent_runs, decode_extent_runs, parse_list_entries,
    visit_list_stream,
};
use crate::ondisk::fixup::apply_fixup;
use crate::ondisk::ntfs::{NtfsAttributeType, NtfsFile, NtfsFileNamespace};
use crate::ondisk::record::attributes_complete;
use crate::scan::{open_raw_volume, volume_geometry};
use crate::volume_label::VolumeLabel;

/// Hard failure from the OS-facing journal/volume layer (unrecoverable here;
/// distinct from the recoverable journal-gone conditions in [`JournalGone`]).
#[derive(Debug, Error)]
pub enum UsnError {
    /// Opening the volume handle failed: the volume path (`\\.\C:`) and the
    /// raw win32 error code.
    #[error("cannot open volume {0} (win32 error {1})")]
    OpenVolume(String, u32),
    /// A `DeviceIoControl`/FSCTL call failed, carrying the raw win32 error code.
    #[error("FSCTL failed (win32 error {0})")]
    Fsctl(u32),
}

/// Why the journal can no longer be tailed; all of these mean "fall back to
/// a full rescan" (docs/RESEARCH.md established practice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalGone {
    /// The requested USN range was overwritten/purged (`ERROR_JOURNAL_ENTRY_DELETED`).
    EntryDeleted,
    /// The journal is being deleted (`ERROR_JOURNAL_DELETE_IN_PROGRESS`).
    DeleteInProgress,
    /// No active journal exists on the volume (`ERROR_JOURNAL_NOT_ACTIVE`).
    NotActive,
    /// The journal id no longer matches the persisted checkpoint (journal was
    /// recreated; surfaced as `ERROR_INVALID_PARAMETER`).
    IdMismatch,
}

/// Result of one blocking journal read: either a parsed batch of records or a
/// recoverable signal that the journal can no longer be tailed.
pub enum ReadOutcome {
    /// A batch of parsed records from the journal buffer.
    Records {
        /// The parsed USN records, in journal order.
        records: Vec<UsnRecord>,
        /// The payload or its next-USN cursor was malformed. Callers must
        /// discard the complete batch and rescan; applying a valid prefix
        /// would skip the dropped suffix forever.
        truncated: bool,
    },
    /// The journal can no longer be tailed; the caller falls back to a rescan.
    Gone(JournalGone),
}

/// How long `read_blocking` parks on a quiet journal before returning a
/// benign empty batch so the worker can re-check its stop flag. Bounds both
/// shutdown latency (one park tick per volume) and change-reflection latency
/// (worst case: this + the 200 ms `IndexChanged` debounce, well inside the
/// ≤1 s budget).
const IDLE_PARK: std::time::Duration = std::time::Duration::from_millis(250);
const FILE_REFERENCE_RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Publish a parsed cursor only for a complete, representable FSCTL batch.
/// Returning false makes the worker discard the whole batch and rescan.
fn advance_complete_cursor(current: &mut i64, next: u64, truncated: bool) -> bool {
    if truncated {
        return false;
    }
    let Ok(next) = i64::try_from(next) else {
        return false;
    };
    if next < *current {
        return false;
    }
    *current = next;
    true
}

fn parse_returned_buffer(buf: &[u8], returned: u32) -> (u64, Vec<UsnRecord>, bool) {
    let returned = returned as usize;
    if returned > buf.len() {
        return (0, Vec::new(), true);
    }
    parse_buffer(&buf[..returned])
}

/// An open USN journal positioned for tailing: the volume handle plus the
/// current replay cursor.
pub struct UsnJournal {
    handle: OwnedHandle,
    /// The journal's identity (`UsnJournalID`); changes if NTFS recreates it,
    /// which invalidates any persisted checkpoint.
    pub journal_id: u64,
    /// The next USN to read from; advances past each returned batch.
    pub next_usn: i64,
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn open_volume_handle(label: VolumeLabel) -> Result<OwnedHandle, UsnError> {
    let path = label.raw_path();
    let wpath = wide(&path);
    unsafe {
        let h = CreateFileW(
            wpath.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return Err(UsnError::OpenVolume(path, GetLastError()));
        }
        Ok(OwnedHandle::from_raw_handle(h as RawHandle))
    }
}

impl UsnJournal {
    /// Open the journal for tailing. Creates it when missing (requires
    /// elevation, which the whole scan path already needs). `start_usn` is
    /// the persisted checkpoint; pass `None` to start at the current end.
    ///
    /// # Errors
    ///
    /// Returns [`UsnError::OpenVolume`] if the volume handle cannot be opened,
    /// or [`UsnError::Fsctl`] if creating or querying the journal fails.
    pub fn open(drive: &str, start_usn: Option<i64>) -> Result<Self, UsnError> {
        let label = VolumeLabel::parse(drive).ok_or(UsnError::Fsctl(ERROR_INVALID_PARAMETER))?;
        let handle = open_volume_handle(label)?;
        let data = Self::query_or_create(&handle)?;
        let next = match start_usn {
            Some(usn) => usn,
            None => data.NextUsn,
        };
        Ok(Self {
            handle,
            journal_id: data.UsnJournalID,
            next_usn: next,
        })
    }

    /// True if the persisted checkpoint is still replayable from this journal.
    #[must_use]
    pub const fn checkpoint_valid(&self, persisted_journal_id: u64, data_first_usn: i64) -> bool {
        self.journal_id == persisted_journal_id && self.next_usn >= data_first_usn
    }

    /// Query the live journal metadata (id and retained USN range).
    ///
    /// # Errors
    ///
    /// Returns [`UsnError::Fsctl`] if the `FSCTL_QUERY_USN_JOURNAL` call fails
    /// (including a journal that is no longer active).
    pub fn query(&self) -> Result<USN_JOURNAL_DATA_V0, UsnError> {
        Self::query_raw(&self.handle).map_err(|e| match e {
            QueryErr::Gone => UsnError::Fsctl(ERROR_JOURNAL_NOT_ACTIVE),
            QueryErr::Os(code) => UsnError::Fsctl(code),
        })
    }

    fn query_or_create(handle: &OwnedHandle) -> Result<USN_JOURNAL_DATA_V0, UsnError> {
        match Self::query_raw(handle) {
            Ok(d) => Ok(d),
            Err(QueryErr::Gone) => {
                // 0 = let NTFS pick defaults (typically 32MB max).
                let create = CREATE_USN_JOURNAL_DATA {
                    MaximumSize: 0,
                    AllocationDelta: 0,
                };
                unsafe {
                    let mut returned = 0u32;
                    let ok = DeviceIoControl(
                        raw(handle),
                        FSCTL_CREATE_USN_JOURNAL,
                        (&raw const create).cast(),
                        size_of::<CREATE_USN_JOURNAL_DATA>() as u32,
                        std::ptr::null_mut(),
                        0,
                        &raw mut returned,
                        std::ptr::null_mut(),
                    );
                    if ok == 0 {
                        return Err(UsnError::Fsctl(GetLastError()));
                    }
                }
                Self::query_raw(handle).map_err(|e| match e {
                    QueryErr::Os(code) => UsnError::Fsctl(code),
                    QueryErr::Gone => UsnError::Fsctl(ERROR_JOURNAL_NOT_ACTIVE),
                })
            }
            Err(QueryErr::Os(code)) => Err(UsnError::Fsctl(code)),
        }
    }

    fn query_raw(handle: &OwnedHandle) -> Result<USN_JOURNAL_DATA_V0, QueryErr> {
        unsafe {
            let mut data: USN_JOURNAL_DATA_V0 = std::mem::zeroed();
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                raw(handle),
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                (&raw mut data).cast(),
                size_of::<USN_JOURNAL_DATA_V0>() as u32,
                &raw mut returned,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                let code = GetLastError();
                return Err(match code {
                    ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => QueryErr::Gone,
                    other => QueryErr::Os(other),
                });
            }
            if returned < size_of::<USN_JOURNAL_DATA_V0>() as u32 {
                return Err(QueryErr::Os(ERROR_INVALID_PARAMETER));
            }
            Ok(data)
        }
    }

    /// Tail read with a bounded park: returns any records available right now,
    /// advancing `next_usn` past them; on a quiet journal it parks for
    /// `IDLE_PARK` (250 ms) and returns a benign empty batch so the caller can
    /// re-check its stop flag instead of blocking forever.
    ///
    /// This deliberately does **not** use the FSCTL's own blocking mode
    /// (`BytesToWaitFor > 0`): per the `READ_USN_JOURNAL_DATA_V0` contract, a
    /// blocking read "remains outstanding until at least one record is
    /// returned or I/O is canceled" — it never returns on a `Timeout`, so a
    /// volume with zero activity would wedge the worker thread and hang
    /// `engine.shutdown()`'s join (the service-stop / idle-self-stop hang).
    /// Instead we issue a non-blocking read (`BytesToWaitFor == 0`, which
    /// "always returns successfully when the end of the change journal file is
    /// encountered") and park in Rust.
    ///
    /// # Errors
    ///
    /// Returns [`UsnError::Fsctl`] if the `FSCTL_READ_USN_JOURNAL` call fails
    /// for a reason other than a recoverable journal-gone condition (those
    /// are reported through [`ReadOutcome`]).
    pub fn read_blocking(&mut self, buf: &mut Vec<u8>) -> Result<ReadOutcome, UsnError> {
        const BUF: usize = 1 << 16;
        buf.resize(BUF, 0);
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: self.next_usn,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            // Non-blocking: return at end-of-journal instead of waiting. The
            // bounded park below (not the FSCTL) is what caps stop latency.
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: self.journal_id,
        };
        unsafe {
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                raw(&self.handle),
                FSCTL_READ_USN_JOURNAL,
                (&raw const input).cast(),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                buf.as_mut_ptr().cast(),
                BUF as u32,
                &raw mut returned,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                let code = GetLastError();
                return match code {
                    ERROR_JOURNAL_ENTRY_DELETED => Ok(ReadOutcome::Gone(JournalGone::EntryDeleted)),
                    ERROR_JOURNAL_DELETE_IN_PROGRESS => {
                        Ok(ReadOutcome::Gone(JournalGone::DeleteInProgress))
                    }
                    ERROR_JOURNAL_NOT_ACTIVE => Ok(ReadOutcome::Gone(JournalGone::NotActive)),
                    // Returned when UsnJournalID no longer matches.
                    ERROR_INVALID_PARAMETER => Ok(ReadOutcome::Gone(JournalGone::IdMismatch)),
                    other => Err(UsnError::Fsctl(other)),
                };
            }
            let (next, records, truncated) = parse_returned_buffer(buf, returned);
            let complete = advance_complete_cursor(&mut self.next_usn, next, truncated);
            // Quiet journal: the non-blocking read returned only the NextUsn
            // header (no records, `next == StartUsn`). Park briefly so the
            // worker re-checks its stop flag within a bounded window — the
            // benign empty batch lets the worker re-check stop — then return.
            // When records are present we skip the park so a backlog drains in
            // a tight loop.
            if complete && records.is_empty() {
                std::thread::sleep(IDLE_PARK);
            }
            Ok(ReadOutcome::Records {
                records,
                truncated: !complete,
            })
        }
    }
}

enum QueryErr {
    Gone,
    Os(u32),
}

fn raw(h: &impl AsRawHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

/// Live metadata fetcher: reads object stats and the complete set of searchable
/// `$FILE_NAME` links for one exact full FRN. All handles are read-only.
pub(super) struct VolumeMetadataFetcher {
    handle: OwnedHandle,
    stream: Mutex<std::fs::File>,
    record_size: usize,
    sector_size: usize,
    cluster_size: u64,
    volume_size: u64,
    stop: Arc<AtomicBool>,
}

enum FileRecordLookup {
    Present(Vec<u8>),
    Gone,
    Failed,
}

impl VolumeMetadataFetcher {
    /// Open read-only volume handles for per-file metadata lookups by FRN.
    ///
    /// # Errors
    ///
    /// Returns [`UsnError::OpenVolume`] if either read-only volume handle
    /// cannot be opened, or [`UsnError::Fsctl`] if NTFS geometry is invalid.
    pub(super) fn open(drive: &str, stop: Arc<AtomicBool>) -> Result<Self, UsnError> {
        let label = VolumeLabel::parse(drive).ok_or(UsnError::Fsctl(ERROR_INVALID_PARAMETER))?;
        let volume_path = label.raw_path();
        let handle = open_volume_handle(label)?;
        let geometry =
            volume_geometry(&volume_path).map_err(|_| UsnError::Fsctl(ERROR_INVALID_PARAMETER))?;
        let stream = open_raw_volume(&volume_path).map_err(|error| {
            let code = error
                .raw_os_error()
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(ERROR_INVALID_PARAMETER);
            UsnError::OpenVolume(volume_path.clone(), code)
        })?;
        Ok(Self {
            handle,
            stream: Mutex::new(stream),
            record_size: usize::try_from(geometry.file_record_size)
                .map_err(|_| UsnError::Fsctl(ERROR_INVALID_PARAMETER))?,
            sector_size: usize::try_from(geometry.sector_size)
                .map_err(|_| UsnError::Fsctl(ERROR_INVALID_PARAMETER))?,
            cluster_size: geometry.cluster_size,
            volume_size: geometry.volume_size,
            stop,
        })
    }

    pub(super) fn stat(&self, frn: u64) -> Option<(u64, i64)> {
        if self.stop.load(Ordering::Relaxed) {
            return None;
        }
        unsafe {
            let mut desc: FILE_ID_DESCRIPTOR = std::mem::zeroed();
            desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
            desc.Type = 0; // FileIdType
            desc.Anonymous.FileId = frn as i64;
            let h = OpenFileById(
                raw(&self.handle),
                &raw const desc,
                0, // attributes-only access
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            );
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let h = OwnedHandle::from_raw_handle(h as RawHandle);
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(raw(&h), &raw mut info) == 0 {
                return None;
            }
            let size = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
            let mtime = ((info.ftLastWriteTime.dwHighDateTime as i64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as i64;
            Some((size, mtime))
        }
    }

    fn read_file_record(&self, full_reference: u64) -> FileRecordLookup {
        const RECORD_MASK: u64 = FILE_REFERENCE_RECORD_MASK;

        if self.stop.load(Ordering::Relaxed) {
            return FileRecordLookup::Failed;
        }
        let record_number = full_reference & RECORD_MASK;
        let input = NTFS_FILE_RECORD_INPUT_BUFFER {
            FileReferenceNumber: record_number as i64,
        };
        let Some(output_len) = self
            .record_size
            .checked_sub(1)
            .and_then(|tail| size_of::<NTFS_FILE_RECORD_OUTPUT_BUFFER>().checked_add(tail))
        else {
            return FileRecordLookup::Failed;
        };
        let Ok(output_len_u32) = u32::try_from(output_len) else {
            return FileRecordLookup::Failed;
        };
        let mut output = vec![0u8; output_len];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                raw(&self.handle),
                FSCTL_GET_NTFS_FILE_RECORD,
                (&raw const input).cast(),
                size_of::<NTFS_FILE_RECORD_INPUT_BUFFER>() as u32,
                output.as_mut_ptr().cast(),
                output_len_u32,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return FileRecordLookup::Failed;
        }
        if self.stop.load(Ordering::Relaxed) {
            return FileRecordLookup::Failed;
        }
        let record_offset = std::mem::offset_of!(NTFS_FILE_RECORD_OUTPUT_BUFFER, FileRecordBuffer);
        let returned = returned as usize;
        if returned < record_offset || output.len() < record_offset {
            return FileRecordLookup::Failed;
        }
        let returned_reference = le_i64(&output, 0) as u64;
        if returned_reference & RECORD_MASK != record_number {
            // This FSCTL enumerates downward and may legally return an earlier
            // in-use record. That is never an answer for an exact link refresh.
            return FileRecordLookup::Gone;
        }
        let record_len = le_u32_session(&output, 8) as usize;
        if record_len != self.record_size
            || record_offset
                .checked_add(record_len)
                .is_none_or(|end| end > returned || end > output.len())
        {
            return FileRecordLookup::Failed;
        }
        let mut record = output[record_offset..record_offset + record_len].to_vec();
        if !NtfsFile::is_valid(&record, self.sector_size)
            || !apply_fixup(&mut record, self.sector_size)
            || !attributes_complete(&record)
        {
            return FileRecordLookup::Failed;
        }
        let Some(file) = NtfsFile::parse(record_number, &record, self.sector_size) else {
            return FileRecordLookup::Failed;
        };
        if !file.is_used() || file.reference_number() != full_reference {
            return FileRecordLookup::Gone;
        }
        FileRecordLookup::Present(record)
    }

    fn read_required_record(&self, full_reference: u64) -> Option<Vec<u8>> {
        match self.read_file_record(full_reference) {
            FileRecordLookup::Present(record) => Some(record),
            FileRecordLookup::Gone | FileRecordLookup::Failed => None,
        }
    }

    fn decode_list_extent_record(
        &self,
        number: u64,
        bytes: &[u8],
        entry: ListEntry,
        expected_base: Option<u64>,
    ) -> Option<Vec<StreamRun>> {
        let file = NtfsFile::parse(number, bytes, self.sector_size)?;
        if file.reference_number() != entry.target_reference {
            return None;
        }
        if let Some(base) = expected_base {
            let actual = file.header.base_reference;
            if actual != base {
                return None;
            }
        }
        let mut found = None;
        file.attributes(|attr| {
            if found.is_some()
                || attr.header.type_id != NtfsAttributeType::AttributeList as u32
                || attr.header.id != entry.id
                || attr.header.is_non_resident == 0
            {
                return;
            }
            let Some(header) = attr.nonresident_header() else {
                return;
            };
            if u64::try_from(header.lowest_vcn).ok() != Some(entry.starting_vcn) {
                return;
            }
            found =
                decode_extent_runs(attr, self.cluster_size, self.volume_size).map(|(_, runs)| runs);
        });
        found
    }

    fn attribute_list_entries(
        &self,
        base: &NtfsFile<'_>,
        record_cache: &mut FxHashMap<u64, Vec<u8>>,
    ) -> Option<Vec<ListEntry>> {
        let attr = base.get_attribute(NtfsAttributeType::AttributeList)?;
        if attr.header.is_non_resident == 0 {
            return parse_list_entries(attr.get_resident()?, false);
        }

        let base_reference = base.reference_number();
        let base_attr_id = attr.header.id;
        let base_lowest_vcn = u64::try_from(attr.nonresident_header()?.lowest_vcn).ok()?;
        let (data_size, base_runs) =
            decode_extent_runs(&attr, self.cluster_size, self.volume_size)?;
        let base_extent = ListEntry::unnamed(
            NtfsAttributeType::AttributeList as u32,
            base_lowest_vcn,
            base_reference,
            base_attr_id,
        );
        let runs = close_extent_runs(
            base_runs,
            data_size,
            base_extent,
            |runs, prefix_len| {
                let mut entries = Vec::new();
                visit_list_stream(
                    &mut *self.stream.lock(),
                    runs,
                    prefix_len,
                    &self.stop,
                    true,
                    |entry| {
                        if entry.type_id == NtfsAttributeType::AttributeList as u32 {
                            entries.push(entry);
                        }
                    },
                )
                .ok()?;
                Some(entries)
            },
            |entry| {
                let number = entry.target_record();
                if number == base.number {
                    self.decode_list_extent_record(number, base.data, entry, None)
                } else {
                    if let Entry::Vacant(slot) = record_cache.entry(number) {
                        slot.insert(self.read_required_record(entry.target_reference)?);
                    }
                    self.decode_list_extent_record(
                        number,
                        record_cache.get(&number)?,
                        entry,
                        Some(base_reference),
                    )
                }
            },
        )?;
        let mut entries = Vec::new();
        visit_list_stream(
            &mut *self.stream.lock(),
            &runs,
            data_size,
            &self.stop,
            false,
            |entry| entries.push(entry),
        )
        .ok()?;
        Some(entries)
    }

    fn link_from_attribute(file: &NtfsFile<'_>, id: Option<u16>, out: &mut Vec<LinkInfo>) -> bool {
        let mut saw_requested = false;
        let mut valid = true;
        file.attributes(|attr| {
            if attr.header.type_id != NtfsAttributeType::FileName as u32 {
                return;
            }
            if attr.header.name_length != 0 || attr.header.flags != 0 {
                valid = false;
                return;
            }
            if id.is_some_and(|wanted| attr.header.id != wanted) {
                return;
            }
            saw_requested = true;
            let Some(name) = attr.as_name() else {
                valid = false;
                return;
            };
            let namespace = name.header.namespace;
            if namespace == NtfsFileNamespace::Dos as u8 {
                return;
            }
            if !is_searchable_namespace(namespace) {
                valid = false;
                return;
            }
            if name.utf16le.is_empty() {
                valid = false;
                return;
            }
            out.push(LinkInfo {
                parent_frn: name.header.parent_directory_reference,
                name: name.to_utf16(),
            });
        });
        valid && (id.is_none() || saw_requested)
    }

    fn links_inner(&self, full_reference: u64, base_bytes: &[u8]) -> Option<Vec<LinkInfo>> {
        let number = full_reference & FILE_REFERENCE_RECORD_MASK;
        let base = NtfsFile::parse(number, base_bytes, self.sector_size)?;
        let base_link = base.header.base_reference;
        if base_link != 0 {
            return None;
        }
        let mut links = Vec::new();
        if !Self::link_from_attribute(&base, None, &mut links) {
            return None;
        }

        let mut record_cache = FxHashMap::default();
        if base
            .get_attribute(NtfsAttributeType::AttributeList)
            .is_some()
        {
            let entries = self.attribute_list_entries(&base, &mut record_cache)?;
            let mut base_name_ids = FxHashSet::default();
            base.attributes(|attribute| {
                if attribute.header.type_id == NtfsAttributeType::FileName as u32 {
                    base_name_ids.insert(attribute.header.id);
                }
            });
            if !base_name_ids.iter().all(|id| {
                entries.iter().any(|entry| {
                    entry.type_id == NtfsAttributeType::FileName as u32
                        && entry.target_reference == full_reference
                        && entry.id == *id
                })
            }) {
                return None;
            }
            for entry in entries {
                if entry.type_id != NtfsAttributeType::FileName as u32 {
                    continue;
                }
                let target_number = entry.target_record();
                if target_number == number {
                    if !Self::link_from_attribute(&base, Some(entry.id), &mut links) {
                        return None;
                    }
                    continue;
                }
                if let Entry::Vacant(slot) = record_cache.entry(target_number) {
                    slot.insert(self.read_required_record(entry.target_reference)?);
                }
                let target = NtfsFile::parse(
                    target_number,
                    record_cache.get(&target_number)?,
                    self.sector_size,
                )?;
                let target_base = target.header.base_reference;
                if target.reference_number() != entry.target_reference
                    || target_base != full_reference
                    || !Self::link_from_attribute(&target, Some(entry.id), &mut links)
                {
                    return None;
                }
            }
        }

        let mut unique = FxHashSet::default();
        links.retain(|link| unique.insert((link.parent_frn, link.name.clone())));
        (!links.is_empty()).then_some(links)
    }

    pub(super) fn links(&self, full_reference: u64) -> LinkSnapshot {
        if self.stop.load(Ordering::Relaxed) {
            return LinkSnapshot::Failed;
        }
        let base = match self.read_file_record(full_reference) {
            FileRecordLookup::Present(record) => record,
            FileRecordLookup::Gone => return LinkSnapshot::Gone,
            FileRecordLookup::Failed => return LinkSnapshot::Failed,
        };
        let Some(links) = self.links_inner(full_reference, &base) else {
            return LinkSnapshot::Failed;
        };
        LinkSnapshot::Present(links)
    }
}

fn le_i64(data: &[u8], off: usize) -> i64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&data[off..off + 8]);
    i64::from_le_bytes(out)
}

fn le_u32_session(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testutil::TestDir;

    #[test]
    fn malformed_batch_never_advances_the_replay_cursor() {
        let mut cursor = 41;
        assert!(!advance_complete_cursor(&mut cursor, 99, true));
        assert_eq!(cursor, 41);
        assert!(!advance_complete_cursor(&mut cursor, 0, false));
        assert_eq!(cursor, 41);
        assert!(!advance_complete_cursor(&mut cursor, u64::MAX, false));
        assert_eq!(cursor, 41);
        assert!(advance_complete_cursor(&mut cursor, 42, false));
        assert_eq!(cursor, 42);

        let mut zero = 0;
        assert!(advance_complete_cursor(&mut zero, 0, false));
        assert_eq!(zero, 0);
    }

    #[test]
    fn impossible_device_byte_count_is_a_malformed_batch() {
        let (_, records, malformed) = parse_returned_buffer(&[0u8; 8], 9);
        assert!(records.is_empty());
        assert!(malformed);
    }

    /// Fails closed. `#[ignore]` is what *skips* these tests; reaching the
    /// body without the arming variable means the harness was invoked outside
    /// `just test-admin`, and a silent early return would be indistinguishable
    /// from a real-volume run that actually happened.
    fn require_admin_gate() {
        assert_eq!(
            std::env::var("FMF_ADMIN_TESTS").as_deref(),
            Ok("1"),
            "this ignored real-volume test must run only through `just test-admin`"
        );
    }

    /// Live smoke for the OS-facing session: open the C: journal, query it,
    /// and complete one blocking read. Run from an elevated shell:
    /// Run with `just test-admin` from an elevated terminal.
    #[test]
    #[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
    fn usn_journal_live_open_query_and_one_read() {
        require_admin_gate();
        let mut journal = UsnJournal::open("C:", None).expect("open C: journal (elevated?)");
        assert_ne!(journal.journal_id, 0);

        let data = journal.query().expect("FSCTL_QUERY_USN_JOURNAL");
        assert_eq!(data.UsnJournalID, journal.journal_id);
        assert!(journal.checkpoint_valid(data.UsnJournalID, data.FirstUsn));
        assert!(!journal.checkpoint_valid(data.UsnJournalID.wrapping_add(1), data.FirstUsn));

        // Rewind to the oldest retained USN so the read returns existing
        // history immediately (a stock C: always has retained journal records).
        let first_usn = data.FirstUsn;
        journal.next_usn = first_usn;

        // read_blocking is bounded now (non-blocking FSCTL + IDLE_PARK), so it
        // always returns. Keep the helper-thread + timeout guard anyway: if a
        // regression reintroduces the old FSCTL blocking mode, this fails
        // instead of wedging the suite forever.
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let outcome = journal.read_blocking(&mut buf);
            let _ = tx.send((outcome, journal.next_usn));
        });

        let (outcome, advanced_usn) = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("read_blocking did not return within 30s");
        reader.join().unwrap();

        match outcome.expect("FSCTL_READ_USN_JOURNAL") {
            ReadOutcome::Records { records, truncated } => {
                assert!(!truncated, "live FSCTL buffer flagged as truncated");
                assert!(!records.is_empty(), "blocking read returned no records");
                assert!(
                    advanced_usn > first_usn,
                    "next_usn must advance past the batch"
                );
            }
            ReadOutcome::Gone(gone) => panic!("journal gone during smoke: {gone:?}"),
        }
    }

    /// Regression guard for the service-stop / idle-self-stop hang: on a quiet
    /// journal (cursor at the tip, no tickle to wake it) `read_blocking` must
    /// still return within a bounded time. The old blocking FSCTL mode
    /// (`BytesToWaitFor > 0`) would wedge here forever on a volume with zero
    /// activity, hanging `engine.shutdown()`'s join. Run elevated:
    /// Run with `just test-admin` from an elevated terminal.
    #[test]
    #[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
    fn usn_quiet_journal_read_returns_bounded() {
        require_admin_gate();
        let mut journal = UsnJournal::open("C:", None).expect("open C: journal (elevated?)");
        let data = journal.query().expect("FSCTL_QUERY_USN_JOURNAL");
        // Position at the journal tip: no history to drain, nothing to wait
        // for. This is exactly the quiet-volume shutdown condition.
        journal.next_usn = data.NextUsn;

        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = tx.send(journal.read_blocking(&mut buf));
        });

        // Bound generously (IDLE_PARK is 250 ms). A regression that restores
        // the blocking FSCTL never sends and this times out; the hang is the
        // bug, so the assertion is the *return*, not what it returns. C: is
        // never truly idle, so tolerate either an empty batch or real records.
        let outcome = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("read_blocking on a quiet journal did not return — stop-hang regression");
        reader.join().unwrap();
        match outcome.expect("FSCTL_READ_USN_JOURNAL") {
            ReadOutcome::Records { truncated, .. } => {
                assert!(!truncated, "live FSCTL buffer flagged as truncated");
            }
            ReadOutcome::Gone(gone) => panic!("journal gone during quiet read: {gone:?}"),
        }
    }

    #[test]
    #[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
    fn live_metadata_returns_the_complete_hard_link_set() {
        require_admin_gate();
        let dir = TestDir::new();
        let first = dir.join("hard-link-first.txt");
        let second = dir.join("hard-link-second.txt");
        std::fs::write(&first, b"hard-link fixture").unwrap();
        std::fs::hard_link(&first, &second).unwrap();

        let file = std::fs::File::open(&first).unwrap();
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(raw(&file), &raw mut info) };
        assert_ne!(ok, 0, "GetFileInformationByHandle");
        let full_reference = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);

        let fetcher = VolumeMetadataFetcher::open("C:", Arc::new(AtomicBool::new(false))).unwrap();
        let LinkSnapshot::Present(links) = fetcher.links(full_reference) else {
            panic!("live metadata did not produce an authoritative link set");
        };
        let names: FxHashSet<Vec<u16>> = links.into_iter().map(|link| link.name).collect();
        assert!(names.contains(&"hard-link-first.txt".encode_utf16().collect::<Vec<_>>()));
        assert!(names.contains(&"hard-link-second.txt".encode_utf16().collect::<Vec<_>>()));
    }
}
