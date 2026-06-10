//! Live USN-journal session: volume handle, FSCTL wrappers, blocking reads
//! and the per-file stat fetcher. This is the only OS-facing part of the
//! `usn` module — everything above it works on parsed records.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};

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
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_QUERY_USN_JOURNAL,
    FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};

use super::apply::StatFetcher;
use super::records::{UsnRecord, parse_buffer};

#[derive(Debug, Error)]
pub enum UsnError {
    #[error("cannot open volume {0} (win32 error {1})")]
    OpenVolume(String, u32),
    #[error("FSCTL failed (win32 error {0})")]
    Fsctl(u32),
}

/// Why the journal can no longer be tailed; all of these mean "fall back to
/// a full rescan" (docs/RESEARCH.md定石).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalGone {
    EntryDeleted,
    DeleteInProgress,
    NotActive,
    IdMismatch,
}

pub enum ReadOutcome {
    Records(Vec<UsnRecord>),
    Gone(JournalGone),
}

pub struct UsnJournal {
    handle: OwnedHandle,
    pub journal_id: u64,
    pub next_usn: i64,
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn open_volume_handle(drive: &str) -> Result<OwnedHandle, UsnError> {
    let path = format!(r"\\.\{}", drive.trim_end_matches(['\\', '/']));
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
    pub fn open(drive: &str, start_usn: Option<i64>) -> Result<Self, UsnError> {
        let handle = open_volume_handle(drive)?;
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
    pub fn checkpoint_valid(&self, persisted_journal_id: u64, data_first_usn: i64) -> bool {
        self.journal_id == persisted_journal_id && self.next_usn >= data_first_usn
    }

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
                        (&create as *const CREATE_USN_JOURNAL_DATA).cast(),
                        size_of::<CREATE_USN_JOURNAL_DATA>() as u32,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
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
                (&mut data as *mut USN_JOURNAL_DATA_V0).cast(),
                size_of::<USN_JOURNAL_DATA_V0>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                let code = GetLastError();
                return Err(match code {
                    ERROR_JOURNAL_NOT_ACTIVE => QueryErr::Gone,
                    ERROR_JOURNAL_DELETE_IN_PROGRESS => QueryErr::Gone,
                    other => QueryErr::Os(other),
                });
            }
            Ok(data)
        }
    }

    /// Blocking read: returns once at least one record is available (or the
    /// journal became invalid). Advances `next_usn` past the returned batch.
    pub fn read_blocking(&mut self, buf: &mut Vec<u8>) -> Result<ReadOutcome, UsnError> {
        const BUF: usize = 1 << 16;
        buf.resize(BUF, 0);
        let input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: self.next_usn,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 1, // block until data arrives
            UsnJournalID: self.journal_id,
        };
        unsafe {
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                raw(&self.handle),
                FSCTL_READ_USN_JOURNAL,
                (&input as *const READ_USN_JOURNAL_DATA_V0).cast(),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                buf.as_mut_ptr().cast(),
                BUF as u32,
                &mut returned,
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
            let (next, records) = parse_buffer(&buf[..returned as usize]);
            if next != 0 {
                self.next_usn = next as i64;
            }
            Ok(ReadOutcome::Records(records))
        }
    }
}

enum QueryErr {
    Gone,
    Os(u32),
}

fn raw(h: &OwnedHandle) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    h.as_raw_handle() as HANDLE
}

/// Live stat fetcher: opens the file by FRN on the same volume and reads
/// size + mtime. Read-only, never follows the open with any mutation.
pub struct VolumeStatFetcher {
    handle: OwnedHandle,
}

impl VolumeStatFetcher {
    pub fn open(drive: &str) -> Result<Self, UsnError> {
        Ok(Self {
            handle: open_volume_handle(drive)?,
        })
    }
}

impl StatFetcher for VolumeStatFetcher {
    fn stat(&self, frn: u64) -> Option<(u64, i64)> {
        unsafe {
            let mut desc: FILE_ID_DESCRIPTOR = std::mem::zeroed();
            desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
            desc.Type = 0; // FileIdType
            desc.Anonymous.FileId = frn as i64;
            let h = OpenFileById(
                raw(&self.handle),
                &desc,
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
            if GetFileInformationByHandle(raw(&h), &mut info) == 0 {
                return None;
            }
            let size = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
            let mtime = ((info.ftLastWriteTime.dwHighDateTime as i64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as i64;
            Some((size, mtime))
        }
    }
}
