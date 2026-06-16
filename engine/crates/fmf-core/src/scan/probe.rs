//! I/O strategy probe (`fmf io-probe`).
//!
//! Measurement only: reads the exact chunk plan the scan would, parses
//! nothing, and reports throughput per strategy (ADR-0011).

use std::time::Instant;

use ntfs_reader::errors::NtfsReaderError;

use crate::mft::MftError;

use super::pipeline::{Chunk, SCAN_CHUNK, plan_chunks};
use super::volume_io::mft_layout;

/// I/O strategy to measure for one $MFT read pass (ADR-0011).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoProbeMode {
    /// The production strategy: buffered synchronous reads.
    Buffered,
    /// Buffered + `FILE_FLAG_SEQUENTIAL_SCAN` cache hint.
    Seq,
    /// `FILE_FLAG_NO_BUFFERING`, synchronous (no cache-manager copy).
    NoBuf,
    /// `FILE_FLAG_NO_BUFFERING` + `FILE_FLAG_OVERLAPPED` with `queue_depth`
    /// reads outstanding — tests whether *sequential* multiplexing helps
    /// (parallel random reads are known to serialize in the kernel).
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
/// `NO_BUFFERING` alignment unit: one page satisfies any 512/4096-sector
/// device for offset, length and buffer-address requirements.
const NOBUF_ALIGN: usize = 4096;

/// Page-aligned read buffer (`NO_BUFFERING` requires aligned addresses).
struct AlignedBuf {
    ptr: std::ptr::NonNull<u8>,
    layout: std::alloc::Layout,
}

impl AlignedBuf {
    fn new(size: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(size, NOBUF_ALIGN)
            .expect("NOBUF_ALIGN is a power-of-two alignment");
        // Safety: layout has non-zero size; abort on allocation failure.
        let ptr = std::ptr::NonNull::new(unsafe { std::alloc::alloc(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        Self { ptr, layout }
    }

    const fn as_mut_slice(&mut self) -> &mut [u8] {
        // Safety: owned allocation of layout.size() bytes.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // Safety: same layout as the allocation.
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
const fn aligned_span(c: &Chunk) -> (u64, usize) {
    let start = c.phys & !(NOBUF_ALIGN as u64 - 1);
    let end = (c.phys + c.want as u64).next_multiple_of(NOBUF_ALIGN as u64);
    (start, (end - start) as usize)
}

fn probe_sync(volume_path: &str, chunks: &[Chunk], flags: u32) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = open_with_flags(volume_path, flags)?;
    let no_buffering = flags & FILE_FLAG_NO_BUFFERING != 0;
    let mut buf = AlignedBuf::new(SCAN_CHUNK + 2 * NOBUF_ALIGN);
    let mut total = 0u64;
    for c in chunks {
        let (phys, want) = if no_buffering {
            aligned_span(c)
        } else {
            (c.phys, c.want)
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
}

fn probe_nobuf_overlapped(
    volume_path: &str,
    chunks: &[Chunk],
    queue_depth: usize,
) -> std::io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::CreateEventW;
    const ERROR_IO_PENDING: u32 = 997;

    let file = open_with_flags(volume_path, FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED)?;
    let handle = file.as_raw_handle() as HANDLE;
    let qd = queue_depth.clamp(1, 16);

    let mut slots: Vec<OvSlot> = (0..qd)
        .map(|_| {
            // Safety: plain event creation; null on failure handled below.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            OvSlot {
                buf: AlignedBuf::new(SCAN_CHUNK + 2 * NOBUF_ALIGN),
                event,
                ov: Box::new(unsafe { std::mem::zeroed::<OVERLAPPED>() }),
                want: 0,
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

    let issue = |slot: &mut OvSlot, c: &Chunk| -> std::io::Result<()> {
        let (phys, want) = aligned_span(c);
        slot.want = want;
        *slot.ov = unsafe { std::mem::zeroed() };
        slot.ov.Anonymous.Anonymous.Offset = (phys & 0xFFFF_FFFF) as u32;
        slot.ov.Anonymous.Anonymous.OffsetHigh = (phys >> 32) as u32;
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
        Ok(transferred as u64)
    };

    // Completions are awaited in issue order — chunk order is what the
    // parse pipeline would need anyway.
    let mut total = 0u64;
    let result = (|| -> std::io::Result<u64> {
        for (i, c) in chunks.iter().enumerate() {
            let slot_idx = i % qd;
            if i >= qd {
                total += wait(&mut slots[slot_idx])?;
            }
            issue(&mut slots[slot_idx], c)?;
        }
        let issued = chunks.len();
        for done in issued.saturating_sub(qd)..issued {
            total += wait(&mut slots[done % qd])?;
        }
        Ok(total)
    })();
    for s in &slots {
        unsafe { CloseHandle(s.event) };
    }
    result
}

/// Measure one $MFT read pass under `mode`. Elevation required (the same
/// volume-handle rule as the scan).
///
/// # Errors
///
/// Returns [`MftError::NotElevated`] when the process lacks the privileges to
/// open the raw volume, or [`MftError::Ntfs`] if reading the $MFT layout or
/// the measured read pass fails.
pub fn io_probe(
    drive: &str,
    mode: IoProbeMode,
    queue_depth: usize,
) -> Result<ProbeStats, MftError> {
    let drive = drive.trim_end_matches(['\\', '/']);
    let volume_path = format!(r"\\.\{drive}");
    let (record_size, data_size, runmap) = mft_layout(&volume_path).map_err(|e| match e {
        NtfsReaderError::ElevationError => MftError::NotElevated,
        other => MftError::Ntfs(other),
    })?;
    let chunks = plan_chunks(&runmap, data_size, record_size);

    let t = Instant::now();
    let bytes = match mode {
        IoProbeMode::Buffered => probe_sync(&volume_path, &chunks, 0),
        IoProbeMode::Seq => probe_sync(&volume_path, &chunks, FILE_FLAG_SEQUENTIAL_SCAN),
        IoProbeMode::NoBuf => probe_sync(&volume_path, &chunks, FILE_FLAG_NO_BUFFERING),
        IoProbeMode::NoBufOverlapped => probe_nobuf_overlapped(&volume_path, &chunks, queue_depth),
    }
    .map_err(|e| MftError::Ntfs(e.into()))?;
    let elapsed = t.elapsed();
    Ok(ProbeStats {
        bytes,
        elapsed_ms: elapsed.as_millis() as u64,
        mb_per_s: bytes as f64 / (1 << 20) as f64 / elapsed.as_secs_f64().max(1e-9),
    })
}
