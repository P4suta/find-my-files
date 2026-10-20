//! I/O strategy probe (`fmf io-probe`).
//!
//! Measurement only: reads the exact chunk plan the scan would, parses nothing, and reports throughput per strategy (ADR-0011).

const MIN_ELAPSED_SECONDS: f64 = 1e-9;
const MEBIBYTE_SHIFT: u32 = 20;
const MAX_QUEUE_DEPTH: usize = 16;

use std::time::Instant;

use crate::mft::MftError;

use super::pipeline::{SCAN_CHUNK, plan_chunks};
use super::volume_io::{ReadSpan, mft_layout};
use crate::ondisk::ntfs::NtfsError;

/// I/O strategy to measure for one $MFT read pass (ADR-0011).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoProbeMode {
    /// The production strategy: buffered synchronous reads.
    Buffered,
    /// Buffered + `FILE_FLAG_SEQUENTIAL_SCAN` cache hint.
    Seq,
    /// `FILE_FLAG_NO_BUFFERING`, synchronous (no cache-manager copy).
    NoBuf,
    /// `FILE_FLAG_NO_BUFFERING` + `FILE_FLAG_OVERLAPPED` with `queue_depth` reads outstanding — tests whether *sequential* multiplexing helps (parallel random reads are known to serialize in the kernel).
    NoBufOverlapped,
}

/// Throughput result of one measured $MFT read pass.
pub struct ProbeStats {
    /// Bytes read during the measured pass.
    pub bytes: u64,
    /// Wall-clock duration of the read pass, in milliseconds.
    pub elapsed_ms: u64,
    /// Throughput in mebibytes per second (`bytes` over elapsed seconds).
    pub mb_per_s: f64,
}

const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
/// `NO_BUFFERING` alignment unit: one page satisfies any 512/4096-sector device for offset, length and buffer-address requirements.
const NOBUF_ALIGN: usize = 4096;
const BUFFER_BYTES: usize = SCAN_CHUNK + 2 * NOBUF_ALIGN;

/// Page-aligned read buffer (`NO_BUFFERING` requires aligned addresses).
struct AlignedBuf {
    ptr: std::ptr::NonNull<u8>,
    layout: std::alloc::Layout,
}

impl AlignedBuf {
    fn new() -> std::io::Result<Self> {
        let layout = std::alloc::Layout::from_size_align(BUFFER_BYTES, NOBUF_ALIGN)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        /* SAFETY: The fixed buffer layout has nonzero size and valid alignment. */
        let ptr = std::ptr::NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        Ok(Self { ptr, layout })
    }

    const fn as_mut_slice(&mut self) -> &mut [u8] {
        /* SAFETY: The owner exclusively borrows all initialized bytes of its allocation. */
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        /* SAFETY: The allocation is owned and freed once with its original layout. */
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

fn open_with_flags(volume_path: &str, flags: u32) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(volume_path)
}

/// Aligned (offset, length) pair covering a chunk under `NO_BUFFERING`.
const fn aligned_span(read: &ReadSpan) -> (u64, usize) {
    let start = read.physical & !(NOBUF_ALIGN as u64 - 1);
    let end = (read.physical + read.len as u64).next_multiple_of(NOBUF_ALIGN as u64);
    (start, (end - start) as usize)
}

fn probe_sync(volume_path: &str, reads: &[ReadSpan], flags: u32) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = open_with_flags(volume_path, flags)?;
    let no_buffering = flags & FILE_FLAG_NO_BUFFERING != 0;
    let mut buf = AlignedBuf::new()?;
    let mut total = 0u64;
    for read in reads {
        let (phys, want) = if no_buffering {
            aligned_span(read)
        } else {
            (read.physical, read.len)
        };
        file.seek(SeekFrom::Start(phys))?;
        file.read_exact(&mut buf.as_mut_slice()[..want])?;
        total += want as u64;
    }
    Ok(total)
}

/// One overlapped read slot: its buffer, its event, its OVERLAPPED block.
struct OvSlot {
    buf: AlignedBuf,
    event: windows_sys::Win32::Foundation::HANDLE,
    ov: Box<windows_sys::Win32::System::IO::OVERLAPPED>,
    want: usize,
    /// True while a read on this slot is outstanding (issued, not yet waited).
    /// Drives the error-path drain: a slot's buffer must not be freed while the kernel may still DMA into it.
    issued: bool,
}

fn probe_nobuf_overlapped(
    volume_path: &str,
    reads: &[ReadSpan],
    queue_depth: usize,
) -> std::io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::CreateEventW;
    const ERROR_IO_PENDING: u32 = 997;

    let file = open_with_flags(volume_path, FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED)?;
    let handle = file.as_raw_handle() as HANDLE;
    let qd = queue_depth.clamp(1, MAX_QUEUE_DEPTH);

    let buffers = (0..qd)
        .map(|_| AlignedBuf::new())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut slots: Vec<OvSlot> = buffers
        .into_iter()
        .map(|buf| {
            // Safety: plain event creation; null on failure handled below.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            OvSlot {
                buf,
                event,
                ov: Box::new(unsafe { std::mem::zeroed::<OVERLAPPED>() }),
                want: 0,
                issued: false,
            }
        })
        .collect();
    if slots.iter().any(|s| s.event.is_null()) {
        for s in &slots {
            if !s.event.is_null() {
                unsafe { CloseHandle(s.event) };
            }
        }
        return Err(std::io::Error::last_os_error());
    }

    let issue = |slot: &mut OvSlot, read: &ReadSpan| -> std::io::Result<()> {
        let (phys, want) = aligned_span(read);
        slot.want = want;
        *slot.ov = unsafe { std::mem::zeroed() };
        slot.ov.Anonymous.Anonymous.Offset = (phys & u64::from(u32::MAX)) as u32;
        slot.ov.Anonymous.Anonymous.OffsetHigh = (phys >> u32::BITS) as u32;
        slot.ov.hEvent = slot.event;
        // Safety: buffer outlives the I/O (slot waits before reuse/drop).
        let ok = unsafe {
            ReadFile(
                handle,
                slot.buf.as_mut_slice().as_mut_ptr(),
                want as u32,
                std::ptr::null_mut(),
                &raw mut *slot.ov,
            )
        };
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(std::io::Error::last_os_error());
        }
        slot.issued = true;
        Ok(())
    };
    let wait = |slot: &mut OvSlot| -> std::io::Result<u64> {
        let mut transferred = 0u32;
        // Safety: the OVERLAPPED belongs to an issued read on this handle.
        let ok =
            unsafe { GetOverlappedResult(handle, &raw const *slot.ov, &raw mut transferred, 1) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        slot.issued = false;
        Ok(transferred as u64)
    };

    let mut total = 0u64;
    let result = (|| -> std::io::Result<u64> {
        for (i, read) in reads.iter().enumerate() {
            let slot_idx = i % qd;
            if i >= qd {
                total += wait(&mut slots[slot_idx])?;
            }
            issue(&mut slots[slot_idx], read)?;
        }
        let issued = reads.len();
        for done in issued.saturating_sub(qd)..issued {
            total += wait(&mut slots[done % qd])?;
        }
        Ok(total)
    })();
    /* INVARIANT: Every issued read completes before its slot, event, or buffer is released, including cancellation paths. */
    if result.is_err() {
        // Safety: `handle` is the open volume handle for this scope.
        unsafe { CancelIo(handle) };
        for s in &slots {
            if s.issued {
                let mut transferred = 0u32;
                /* SAFETY: The OVERLAPPED belongs to an issued read on the live handle and its event remains open until completion.
                 * ERROR_OPERATION_ABORTED is expected after cancellation, so the completion status is ignored.
                 */
                let _ = unsafe {
                    GetOverlappedResult(handle, &raw const *s.ov, &raw mut transferred, 1)
                };
            }
        }
    }
    for s in &slots {
        unsafe { CloseHandle(s.event) };
    }
    result
}

/// Measure one $MFT read pass under `mode`.
/// Elevation required (the same volume-handle rule as the scan).
///
/// # Errors
///
/// Returns [`MftError::NotElevated`] when the process lacks the privileges to open the raw volume, or [`MftError::Ntfs`] if reading the $MFT layout or the measured read pass fails.
pub fn io_probe(
    drive: &str,
    mode: IoProbeMode,
    queue_depth: usize,
) -> Result<ProbeStats, MftError> {
    let label = crate::volume_label::VolumeLabel::parse(drive).ok_or_else(|| {
        MftError::Ntfs("volume label must be exactly one ASCII drive letter and ':'".to_string())
    })?;
    let volume_path = label.raw_path();
    let layout = mft_layout(&volume_path).map_err(MftError::from)?;
    let chunks =
        plan_chunks(&layout.runmap, layout.data_size, layout.record_size).ok_or_else(|| {
            MftError::Ntfs("$MFT length is not a whole number of file records".to_string())
        })?;
    let reads: Vec<ReadSpan> = chunks.into_iter().flat_map(|chunk| chunk.reads).collect();

    let t = Instant::now();
    let bytes = match mode {
        IoProbeMode::Buffered => probe_sync(&volume_path, &reads, 0),
        IoProbeMode::Seq => probe_sync(&volume_path, &reads, FILE_FLAG_SEQUENTIAL_SCAN),
        IoProbeMode::NoBuf => probe_sync(&volume_path, &reads, FILE_FLAG_NO_BUFFERING),
        IoProbeMode::NoBufOverlapped => probe_nobuf_overlapped(&volume_path, &reads, queue_depth),
    }
    .map_err(NtfsError::from)
    .map_err(MftError::from)?;
    let elapsed = t.elapsed();
    Ok(ProbeStats {
        bytes,
        elapsed_ms: elapsed.as_millis() as u64,
        mb_per_s: bytes as f64
            / (1 << MEBIBYTE_SHIFT) as f64
            / elapsed.as_secs_f64().max(MIN_ELAPSED_SECONDS),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_read_buffers_are_initialized_and_page_aligned() {
        let mut buffer = AlignedBuf::new().expect("fixed read-buffer allocation");
        let bytes = buffer.as_mut_slice();
        assert_eq!(bytes.len(), BUFFER_BYTES);
        assert_eq!(bytes.as_ptr() as usize % NOBUF_ALIGN, 0);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
}
