//! Read-ahead pipeline (ADR-0011): record-aligned chunk planning over the
//! $MFT run map, plus the dedicated I/O thread that reads chunk N+1 while
//! chunk N parses. If the thread can't start, the scan degrades to inline
//! sequential reads (`scan_pipeline_fallbacks`).

use std::time::{Duration, Instant};
use std::{
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};

use super::volume_io::{ReadSpan, RunMap, open_shared_read};
use crate::ondisk::ntfs::NtfsError;

pub(super) const SCAN_CHUNK: usize = 16 << 20;
/// Chunk buffers cycling between the I/O thread and the parser (one being
/// read, one queued, one being parsed) — bounds peak RAM at 3 chunks.
const PIPELINE_BUFFERS: usize = 3;

/// Record-aligned read unit of the $MFT data stream.
#[derive(Clone)]
pub(super) struct Chunk {
    pub(super) logical: u64,
    pub(super) want: usize,
    pub(super) reads: Vec<ReadSpan>,
}

/// Terminal state of a scan pipeline.
pub(super) enum PipelineOutcome {
    /// Every planned chunk was delivered to the parser.
    Complete { read_time: Duration, fallbacks: u64 },
    /// The owner requested shutdown. No partially built index may be
    /// published after this outcome.
    Cancelled,
}

/// Pure chunk-plan arithmetic: record-aligned logical chunks whose physical
/// reads may cross any number of fragmented data runs. Sparse spans remain
/// zero-filled in the destination.
pub(super) fn plan_chunks(map: &RunMap, data_size: u64, record_size: usize) -> Option<Vec<Chunk>> {
    let record_size_u64 = u64::try_from(record_size).ok()?;
    if record_size == 0 || !data_size.is_multiple_of(record_size_u64) {
        return None;
    }
    let mut chunks = Vec::new();
    let mut logical = 0u64;
    while logical < data_size {
        let remaining = usize::try_from(data_size - logical).ok()?;
        let want = SCAN_CHUNK.min(remaining) / record_size * record_size;
        if want == 0 {
            return None;
        }
        chunks.push(Chunk {
            logical,
            want,
            reads: map.data_spans(logical, want)?,
        });
        logical = logical.checked_add(u64::try_from(want).ok()?)?;
    }
    Some(chunks)
}

fn read_chunk(
    file: &mut std::fs::File,
    chunk: &Chunk,
    buffer: &mut [u8],
    stop: &AtomicBool,
) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};

    let output = &mut buffer[..chunk.want];
    output.fill(0);
    for span in &chunk.reads {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let end = span
            .output_offset
            .checked_add(span.len)
            .ok_or_else(|| std::io::Error::other("run span overflow"))?;
        if end > output.len() {
            return Err(std::io::Error::other("run span escapes chunk"));
        }
        file.seek(SeekFrom::Start(span.physical))?;
        file.read_exact(&mut output[span.output_offset..end])?;
    }
    Ok(!stop.load(Ordering::Relaxed))
}

/// Read chunks on a dedicated I/O thread while the caller parses the
/// previous one; buffers cycle through a bounded channel pair. Returns the
/// accumulated device-read time and the fallback count (1 when the thread
/// couldn't start and the scan degraded to inline sequential reads).
pub(super) fn run_chunk_pipeline(
    volume_path: &str,
    chunks: &[Chunk],
    stop: &Arc<AtomicBool>,
    on_chunk: &mut dyn FnMut(usize, &mut [u8]),
) -> Result<PipelineOutcome, NtfsError> {
    use std::sync::mpsc::{self, RecvTimeoutError};

    if stop.load(Ordering::Relaxed) {
        return Ok(PipelineOutcome::Cancelled);
    }
    let mut file = open_shared_read(volume_path)?;
    let plan = chunks.to_vec();
    let (full_tx, full_rx) =
        mpsc::sync_channel::<std::io::Result<(usize, Vec<u8>)>>(PIPELINE_BUFFERS);
    let (empty_tx, empty_rx) = mpsc::channel::<Vec<u8>>();
    for _ in 0..PIPELINE_BUFFERS {
        let _ = empty_tx.send(vec![0u8; SCAN_CHUNK]);
    }

    let io_stop = Arc::clone(stop);
    let spawned = std::thread::Builder::new()
        .name("fmf-scan-io".into())
        .spawn(move || {
            let mut read_time = Duration::ZERO;
            for (i, chunk) in plan.iter().enumerate() {
                if io_stop.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut buf) = empty_rx.recv() else {
                    break; // parser side gone (error path) — stop reading
                };
                if io_stop.load(Ordering::Relaxed) {
                    break;
                }
                let t = Instant::now();
                let read = read_chunk(&mut file, chunk, &mut buf, &io_stop);
                read_time += t.elapsed();
                match read {
                    Ok(true) => {
                        if full_tx.send(Ok((i, buf))).is_err() {
                            break;
                        }
                    }
                    Ok(false) => break,
                    Err(error) => {
                        let _ = full_tx.send(Err(error));
                        break;
                    }
                }
            }
            read_time
        });

    let Ok(handle) = spawned else {
        // Degraded but correct: read inline on this thread. The original
        // handle moved into the dead closure, so open a fresh one. The
        // worker records the returned fallback count and emits the one
        // diagnostic for the scan.
        let mut file = open_shared_read(volume_path)?;
        let mut buf = vec![0u8; SCAN_CHUNK];
        let mut read_time = Duration::ZERO;
        for (i, c) in chunks.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                return Ok(PipelineOutcome::Cancelled);
            }
            let t = Instant::now();
            if !read_chunk(&mut file, c, &mut buf, stop)? {
                return Ok(PipelineOutcome::Cancelled);
            }
            read_time += t.elapsed();
            if stop.load(Ordering::Relaxed) {
                return Ok(PipelineOutcome::Cancelled);
            }
            on_chunk(i, &mut buf[..c.want]);
        }
        return Ok(PipelineOutcome::Complete {
            read_time,
            fallbacks: 1,
        });
    };

    let mut result: Result<(), NtfsError> = Ok(());
    let mut received = 0usize;
    let mut cancelled = false;
    while received < chunks.len() {
        if stop.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        match full_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok((i, mut buf))) => {
                if stop.load(Ordering::Relaxed) {
                    cancelled = true;
                    break;
                }
                on_chunk(i, &mut buf[..chunks[i].want]);
                received += 1;
                let _ = empty_tx.send(buf);
            }
            Ok(Err(e)) => {
                result = Err(e.into());
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                result = Err(std::io::Error::other("scan I/O thread terminated early").into());
                break;
            }
        }
    }
    // Unblock the thread (its send/recv fail once these drop), then join.
    drop(full_rx);
    drop(empty_tx);
    let read_time = handle.join().unwrap_or(Duration::ZERO);
    if cancelled || stop.load(Ordering::Relaxed) {
        Ok(PipelineOutcome::Cancelled)
    } else {
        result.map(|()| PipelineOutcome::Complete {
            read_time,
            fallbacks: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testutil::TestDir;

    /// Pins `plan_chunks` arithmetic: record alignment, sparse zero-fill,
    /// and full logical coverage in order.
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
        let chunks = plan_chunks(&map, data_size, rs).unwrap();

        assert!(!chunks.is_empty());
        let mut prev_end = 0u64;
        for c in &chunks {
            assert_eq!(c.want % rs, 0, "chunk not record-aligned");
            assert!(c.want <= SCAN_CHUNK);
            assert!(c.logical >= prev_end, "chunks out of order");
            prev_end = c.logical + c.want as u64;
        }
        let covered: usize = chunks.iter().map(|c| c.want).sum();
        assert_eq!(covered as u64, data_size);
    }

    #[test]
    fn plan_rejects_a_partial_file_record() {
        let map = RunMap {
            runs: vec![(0, 0, 1536)],
        };
        assert!(plan_chunks(&map, 1536, 1024).is_none());
        assert!(plan_chunks(&map, 1024, 0).is_none());
    }

    /// The pipeline works on any file path (the volume handle is just a
    /// file with share flags), so ordering, buffer recycling and the error
    /// path are testable without elevation.
    #[test]
    fn pipeline_delivers_chunks_in_order_with_recycled_buffers() {
        const RUN: u64 = 1536;

        let stop = Arc::new(AtomicBool::new(false));
        let rs = 1024usize;
        let dir = TestDir::new();
        let path = dir.join("stream.bin");
        // 8 runs of 1536 bytes each, deliberately not in physical order.
        // Every other boundary splits a 1KiB file record.
        let total = 12 * 1024usize;
        let bytes: Vec<u8> = (0..total).map(|i| (i / 7 % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mut runs = Vec::new();
        for i in 0..8u64 {
            let phys = ((i + 3) % 8) * RUN; // scrambled physical layout
            runs.push((i * RUN, phys, RUN));
        }
        let map = RunMap { runs };
        let chunks = plan_chunks(&map, total as u64, rs).unwrap();
        assert_eq!(
            chunks.len(),
            1,
            "physical runs gather into one logical chunk"
        );

        let mut expected = vec![0u8; total];
        for &(logical, physical, len) in &map.runs {
            expected[logical as usize..(logical + len) as usize]
                .copy_from_slice(&bytes[physical as usize..(physical + len) as usize]);
        }

        let mut seen = Vec::new();
        let outcome = run_chunk_pipeline(path.to_str().unwrap(), &chunks, &stop, &mut |i, got| {
            assert_eq!(
                got, expected,
                "logical bytes must cross physical run boundaries"
            );
            seen.push(i);
        })
        .expect("pipeline");
        let PipelineOutcome::Complete {
            read_time,
            fallbacks,
        } = outcome
        else {
            panic!("pipeline unexpectedly cancelled");
        };
        assert_eq!(seen, vec![0], "strict chunk order");
        assert_eq!(fallbacks, 0);
        assert!(read_time <= std::time::Duration::from_secs(5));
    }

    #[test]
    fn pipeline_propagates_read_errors() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = TestDir::new();
        let path = dir.join("short.bin");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        // Plan claims 4KiB at physical 0 — read_exact must fail past EOF and
        // the error must surface instead of hanging the channel pair.
        let chunks = vec![Chunk {
            logical: 0,
            want: 4096,
            reads: vec![ReadSpan {
                output_offset: 0,
                physical: 0,
                len: 4096,
            }],
        }];
        let mut called = 0;
        let r = run_chunk_pipeline(path.to_str().unwrap(), &chunks, &stop, &mut |_, _| {
            called += 1;
        });
        assert!(r.is_err());
        assert_eq!(called, 0);
    }

    #[test]
    fn pipeline_stops_after_the_current_bounded_chunk() {
        let stop = Arc::new(AtomicBool::new(false));
        let dir = TestDir::new();
        let path = dir.join("cancel.bin");
        let bytes = vec![0xA5; 8 * 4096];
        std::fs::write(&path, bytes).unwrap();
        let chunks: Vec<Chunk> = (0..8)
            .map(|i| Chunk {
                logical: i * 4096,
                want: 4096,
                reads: vec![ReadSpan {
                    output_offset: 0,
                    physical: i * 4096,
                    len: 4096,
                }],
            })
            .collect();

        let mut called = 0;
        let outcome = run_chunk_pipeline(path.to_str().unwrap(), &chunks, &stop, &mut |_, _| {
            called += 1;
            stop.store(true, Ordering::Relaxed);
        })
        .expect("cancellation is not an I/O error");

        assert!(matches!(outcome, PipelineOutcome::Cancelled));
        assert_eq!(called, 1, "no chunk after cancellation may be parsed");
    }
}
