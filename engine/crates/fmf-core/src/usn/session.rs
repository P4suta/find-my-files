//! Live USN-journal session: volume handle, FSCTL wrappers, blocking reads
//! and the per-file stat fetcher.
//!
//! This is the only OS-facing part of the `usn` module — everything above it
//! works on parsed records.

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
        /// Trailing bytes were malformed and dropped — surfaced as a
        /// counter + warning by the caller.
        truncated: bool,
    },
    /// The journal can no longer be tailed; the caller falls back to a rescan.
    Gone(JournalGone),
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
    ///
    /// # Errors
    ///
    /// Returns [`UsnError::OpenVolume`] if the volume handle cannot be opened,
    /// or [`UsnError::Fsctl`] if creating or querying the journal fails.
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
            Ok(data)
        }
    }

    /// Blocking read: returns once at least one record is available (or the
    /// journal became invalid). Advances `next_usn` past the returned batch.
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
            Timeout: 0,
            BytesToWaitFor: 1, // block until data arrives
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
            let (next, records, truncated) = parse_buffer(&buf[..returned as usize]);
            if next != 0 {
                self.next_usn = next as i64;
            }
            Ok(ReadOutcome::Records { records, truncated })
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
    failures: std::sync::atomic::AtomicU64,
}

impl VolumeStatFetcher {
    /// Open a read-only volume handle for per-file stat lookups by FRN.
    ///
    /// # Errors
    ///
    /// Returns [`UsnError::OpenVolume`] if the volume handle cannot be opened.
    pub fn open(drive: &str) -> Result<Self, UsnError> {
        Ok(Self {
            handle: open_volume_handle(drive)?,
            failures: std::sync::atomic::AtomicU64::new(0),
        })
    }
}

impl VolumeStatFetcher {
    /// Failures here are expected (file deleted before we look) but a flood
    /// of them is not — count every one, log the first few with the win32
    /// code so the pattern is diagnosable.
    fn note_failure(&self, frn: u64, stage: &str) {
        let n = self
            .failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n < 5 {
            let code = unsafe { GetLastError() };
            tracing::warn!(frn, stage, code, "stat fetch failed");
        }
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
                &raw const desc,
                0, // attributes-only access
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            );
            if h == INVALID_HANDLE_VALUE {
                self.note_failure(frn, "OpenFileById");
                return None;
            }
            let h = OwnedHandle::from_raw_handle(h as RawHandle);
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(raw(&h), &raw mut info) == 0 {
                self.note_failure(frn, "GetFileInformationByHandle");
                return None;
            }
            let size = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
            let mtime = ((info.ftLastWriteTime.dwHighDateTime as i64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as i64;
            Some((size, mtime))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live smoke for the OS-facing session: open the C: journal, query it,
    /// and complete one blocking read. Run from an elevated shell:
    /// `FMF_ADMIN_TESTS=1` cargo test -p fmf-core -- --ignored `usn_journal`
    #[test]
    #[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
    fn usn_journal_live_open_query_and_one_read() {
        if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
            eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
            return;
        }
        let mut journal = UsnJournal::open("C:", None).expect("open C: journal (elevated?)");
        assert_ne!(journal.journal_id, 0);

        let data = journal.query().expect("FSCTL_QUERY_USN_JOURNAL");
        assert_eq!(data.UsnJournalID, journal.journal_id);
        assert!(journal.checkpoint_valid(data.UsnJournalID, data.FirstUsn));
        assert!(!journal.checkpoint_valid(data.UsnJournalID.wrapping_add(1), data.FirstUsn));

        // Rewind to the oldest retained USN so the blocking read returns
        // existing history immediately instead of waiting for new activity.
        let first_usn = data.FirstUsn;
        journal.next_usn = first_usn;

        // read_blocking has no timeout by design; run it on a helper thread
        // and bound the wait here so a regression hangs the test, not the
        // suite. The tickle file (temp dir is on C: on a stock setup) covers
        // the freshly-created-journal case where no history exists yet.
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let outcome = journal.read_blocking(&mut buf);
            let _ = tx.send((outcome, journal.next_usn));
        });
        let tickle = std::env::temp_dir().join("fmf-usn-smoke.tmp");
        let _ = std::fs::write(&tickle, b"tick");
        let _ = std::fs::remove_file(&tickle);

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
}
