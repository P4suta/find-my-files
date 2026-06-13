//! Read-ahead pipeline (ADR-0011): record-aligned chunk planning over the
//! $MFT run map, plus the dedicated I/O thread that reads chunk N+1 while
//! chunk N parses. If the thread can't start, the scan degrades to inline
//! sequential reads (`scan_pipeline_fallbacks`).

use std::time::{Duration, Instant};

use ntfs_reader::errors::NtfsReaderError;

use super::volume_io::{RunMap, open_raw_volume};

pub(super) const SCAN_CHUNK: usize = 16 << 20;
/// Chunk buffers cycling between the I/O thread and the parser (one being
/// read, one queued, one being parsed) — bounds peak RAM at 3 chunks.
const PIPELINE_BUFFERS: usize = 3;

/// Record-aligned read unit of the $MFT data stream.
pub(super) struct Chunk {
    pub(super) logical: u64,
    pub(super) phys: u64,
    pub(super) want: usize,
}

/// Pure chunk-plan arithmetic: record-aligned chunking, sparse-hole
/// skipping, no I/O.
pub(super) fn plan_chunks(map: &RunMap, data_size: u64, record_size: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut logical = 0u64;
    while logical < data_size {
        let Some((phys, contig)) = map.physical(logical) else {
            logical += record_size as u64; // sparse hole: no records here
            continue;
        };
        let want = SCAN_CHUNK
            .min(contig as usize)
            .min((data_size - logical) as usize)
            / record_size
            * record_size;
        if want == 0 {
            logical += record_size as u64;
            continue;
        }
        chunks.push(Chunk {
            logical,
            phys,
            want,
        });
        logical += want as u64;
    }
    chunks
}

/// Read chunks on a dedicated I/O thread while the caller parses the
/// previous one; buffers cycle through a bounded channel pair. Returns the
/// accumulated device-read time and the fallback count (1 when the thread
/// couldn't start and the scan degraded to inline sequential reads).
pub(super) fn run_chunk_pipeline(
    volume_path: &str,
    chunks: &[Chunk],
    on_chunk: &mut dyn FnMut(usize, &mut [u8]),
) -> Result<(Duration, u64), NtfsReaderError> {
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::mpsc;

    let mut file = open_raw_volume(volume_path)?;
    let plan: Vec<(u64, usize)> = chunks.iter().map(|c| (c.phys, c.want)).collect();
    let (full_tx, full_rx) =
        mpsc::sync_channel::<std::io::Result<(usize, Vec<u8>)>>(PIPELINE_BUFFERS);
    let (empty_tx, empty_rx) = mpsc::channel::<Vec<u8>>();
    for _ in 0..PIPELINE_BUFFERS {
        let _ = empty_tx.send(vec![0u8; SCAN_CHUNK]);
    }

    let spawned = std::thread::Builder::new()
        .name("fmf-scan-io".into())
        .spawn(move || {
            let mut read_time = Duration::ZERO;
            for (i, &(phys, want)) in plan.iter().enumerate() {
                let Ok(mut buf) = empty_rx.recv() else {
                    break; // parser side gone (error path) — stop reading
                };
                let t = Instant::now();
                let read = file
                    .seek(SeekFrom::Start(phys))
                    .and_then(|_| file.read_exact(&mut buf[..want]));
                read_time += t.elapsed();
                let failed = read.is_err();
                if full_tx.send(read.map(|()| (i, buf))).is_err() || failed {
                    break;
                }
            }
            read_time
        });

    let handle = match spawned {
        Ok(h) => h,
        Err(e) => {
            // Degraded but correct: read inline on this thread. The original
            // handle moved into the dead closure, so open a fresh one.
            tracing::warn!(error = %e, "scan I/O thread unavailable — inline sequential reads");
            let mut file = open_raw_volume(volume_path)?;
            let mut buf = vec![0u8; SCAN_CHUNK];
            let mut read_time = Duration::ZERO;
            for (i, c) in chunks.iter().enumerate() {
                let t = Instant::now();
                file.seek(SeekFrom::Start(c.phys))?;
                file.read_exact(&mut buf[..c.want])?;
                read_time += t.elapsed();
                on_chunk(i, &mut buf[..c.want]);
            }
            return Ok((read_time, 1));
        }
    };

    let mut result: Result<(), NtfsReaderError> = Ok(());
    for _ in 0..chunks.len() {
        match full_rx.recv() {
            Ok(Ok((i, mut buf))) => {
                on_chunk(i, &mut buf[..chunks[i].want]);
                let _ = empty_tx.send(buf);
            }
            Ok(Err(e)) => {
                result = Err(e.into());
                break;
            }
            Err(_) => {
                result = Err(std::io::Error::other("scan I/O thread terminated early").into());
                break;
            }
        }
    }
    // Unblock the thread (its send/recv fail once these drop), then join.
    drop(full_rx);
    drop(empty_tx);
    let read_time = handle.join().unwrap_or(Duration::ZERO);
    result.map(|()| (read_time, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testutil::TestDir;

    /// Pins `plan_chunks` arithmetic: record alignment, sparse-hole
    /// skipping, full logical coverage in order.
    #[test]
    fn plan_chunks_is_record_aligned_and_ordered() {
        let rs = 1024usize;
        // Two data runs separated by a sparse hole; second run larger than
        // SCAN_CHUNK to force a split.
        let map = RunMap {
            runs: vec![
                (0, 4096, 8 * 1024),
                (16 * 1024, 0, SCAN_CHUNK as u64 + 4 * 1024),
            ],
        };
        let data_size = 16 * 1024 + SCAN_CHUNK as u64 + 4 * 1024;
        let chunks = plan_chunks(&map, data_size, rs);

        assert!(!chunks.is_empty());
        let mut prev_end = 0u64;
        for c in &chunks {
            assert_eq!(c.want % rs, 0, "chunk not record-aligned");
            assert!(c.want <= SCAN_CHUNK);
            assert!(c.logical >= prev_end, "chunks out of order");
            prev_end = c.logical + c.want as u64;
        }
        let covered: usize = chunks.iter().map(|c| c.want).sum();
        // Everything except the sparse hole gets read.
        assert_eq!(covered as u64, data_size - 8 * 1024);
    }

    /// The pipeline works on any file path (the volume handle is just a
    /// file with share flags), so ordering, buffer recycling and the error
    /// path are testable without elevation.
    #[test]
    fn pipeline_delivers_chunks_in_order_with_recycled_buffers() {
        let rs = 512usize;
        let dir = TestDir::new();
        let path = dir.join("stream.bin");
        // 8 runs of 2KiB each, deliberately not in physical order.
        let total = 16 * 1024usize;
        let bytes: Vec<u8> = (0..total).map(|i| (i / 7 % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mut runs = Vec::new();
        for i in 0..8u64 {
            let phys = ((i + 3) % 8) * 2048; // scrambled physical layout
            runs.push((i * 2048, phys, 2048));
        }
        let map = RunMap { runs };
        let chunks = plan_chunks(&map, total as u64, rs);
        assert_eq!(chunks.len(), 8, "one chunk per contiguous run");

        let mut seen = Vec::new();
        let (read_time, fallbacks) =
            run_chunk_pipeline(path.to_str().unwrap(), &chunks, &mut |i, got| {
                let c = &chunks[i];
                assert_eq!(
                    got,
                    &bytes[c.phys as usize..c.phys as usize + c.want],
                    "chunk {i} bytes must come from its physical offset"
                );
                seen.push(i);
            })
            .expect("pipeline");
        assert_eq!(seen, (0..8).collect::<Vec<_>>(), "strict chunk order");
        assert_eq!(fallbacks, 0);
        assert!(read_time <= std::time::Duration::from_secs(5));
    }

    #[test]
    fn pipeline_propagates_read_errors() {
        let dir = TestDir::new();
        let path = dir.join("short.bin");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        // Plan claims 4KiB at physical 0 — read_exact must fail past EOF and
        // the error must surface instead of hanging the channel pair.
        let chunks = vec![Chunk {
            logical: 0,
            phys: 0,
            want: 4096,
        }];
        let mut called = 0;
        let r = run_chunk_pipeline(path.to_str().unwrap(), &chunks, &mut |_, _| called += 1);
        assert!(r.is_err());
        assert_eq!(called, 0);
    }
}
