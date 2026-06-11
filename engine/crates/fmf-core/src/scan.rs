//! Streaming $MFT scanner (perf plan Workstream C).
//!
//! Replaces the whole-$MFT-in-RAM approach: the $MFT's data runs are read in
//! 16MiB aligned chunks through our own volume handle (large sequential
//! reads run at device speed instead of ntfs-reader's small buffered ones),
//! records are fixed up and parsed per chunk, and the buffers are recycled —
//! peak RAM drops from "size of $MFT" to a few chunks. ntfs-reader still
//! provides the bootstrap (boot-sector geometry + record 0's data runs) and
//! the per-record attribute parsing types.
//!
//! Two layers of overlap (macro-parallel by construction, entry order stays
//! byte-for-byte identical to a sequential scan):
//! - a dedicated I/O thread reads chunk N+1 while chunk N parses
//!   ([`run_chunk_pipeline`]; degrades to inline reads if the thread can't
//!   start — `scan_pipeline_fallbacks`)
//! - within a chunk, record sub-ranges parse on rayon workers that carry the
//!   WTF-8 encoding too ([`parse_chunk`]); the builder then appends the
//!   worker batches in chunk order, so EntryId assignment is deterministic.

use std::time::{Duration, Instant};

use ntfs_reader::api::{
    FIRST_NORMAL_RECORD, NtfsAttributeListEntry, NtfsAttributeType, NtfsFileName,
    NtfsFileNamespace, ROOT_RECORD,
};
use ntfs_reader::errors::NtfsReaderError;
use ntfs_reader::file::NtfsFile;
use ntfs_reader::mft::Mft;
use ntfs_reader::volume::Volume;

use crate::index::{EncodedEntry, VolumeIndex, VolumeIndexBuilder};
use crate::mft::{MftError, ScanStats, peak_working_set, pick_name};
use crate::wtf8;

const SCAN_CHUNK: usize = 16 << 20;
/// Sub-range fed to one parse worker — small enough to spread a 16MiB chunk
/// across cores, large enough to amortize the per-task overhead.
const PARSE_SUB: usize = 1 << 20;
/// Chunk buffers cycling between the I/O thread and the parser (one being
/// read, one queued, one being parsed) — bounds peak RAM at 3 chunks.
const PIPELINE_BUFFERS: usize = 3;
const SECTOR: usize = 512;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Logical-byte → physical-byte mapping of the $MFT data stream.
struct RunMap {
    /// (logical start, physical start, length) — all bytes.
    runs: Vec<(u64, u64, u64)>,
}

impl RunMap {
    fn from_data_runs(runs: &[ntfs_reader::attribute::DataRun]) -> Self {
        use ntfs_reader::attribute::DataRun;
        let mut v = Vec::with_capacity(runs.len());
        let mut logical = 0u64;
        for r in runs {
            match r {
                DataRun::Data { lcn, length } => {
                    v.push((logical, *lcn, *length));
                    logical += length;
                }
                DataRun::Sparse { length } => logical += length,
            }
        }
        RunMap { runs: v }
    }

    /// Physical offset and remaining contiguous bytes at `logical`.
    fn physical(&self, logical: u64) -> Option<(u64, u64)> {
        // Runs are few (usually < 100); linear is fine.
        self.runs
            .iter()
            .find(|(ls, _, len)| logical >= *ls && logical < ls + len)
            .map(|(ls, ph, len)| (ph + (logical - ls), ls + len - logical))
    }
}

fn open_raw_volume(volume_path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(volume_path)
}

/// Apply the NTFS update sequence array in place. Returns false when the
/// sector check bytes don't match (torn/corrupt record).
fn apply_fixup(data: &mut [u8]) -> bool {
    if data.len() < 48 {
        return false;
    }
    let uso = u16::from_le_bytes([data[4], data[5]]) as usize;
    let usl = u16::from_le_bytes([data[6], data[7]]) as usize;
    if usl < 2 || uso + usl * 2 > data.len() {
        return false;
    }
    let usn = [data[uso], data[uso + 1]];
    let mut sector_off = SECTOR - 2;
    for i in 1..usl {
        let usa_off = uso + i * 2;
        if sector_off + 2 > data.len() {
            break;
        }
        if data[sector_off] != usn[0] || data[sector_off + 1] != usn[1] {
            return false;
        }
        data[sector_off] = data[usa_off];
        data[sector_off + 1] = data[usa_off + 1];
        sector_off += SECTOR;
    }
    true
}

/// Random access to single records for the deferred attribute-list pass.
struct RecordReader<'a> {
    file: std::fs::File,
    map: &'a RunMap,
    record_size: usize,
    buf: Vec<u8>,
}

impl RecordReader<'_> {
    fn read_record(&mut self, number: u64) -> Option<&[u8]> {
        use std::io::{Read, Seek, SeekFrom};
        let logical = number * self.record_size as u64;
        let (phys, contig) = self.map.physical(logical)?;
        if (contig as usize) < self.record_size {
            return None;
        }
        self.buf.resize(self.record_size, 0);
        self.file.seek(SeekFrom::Start(phys)).ok()?;
        self.file.read_exact(&mut self.buf).ok()?;
        if !NtfsFile::is_valid(&self.buf) || !apply_fixup(&mut self.buf) {
            return None;
        }
        Some(&self.buf)
    }
}

/// Resolve the display name of a record whose $FILE_NAME lives in extension
/// records (resident $ATTRIBUTE_LIST → referenced records). Mirrors
/// ntfs-reader's get_best_file_name without needing the whole MFT in RAM.
fn resolve_attr_list_name(base: &NtfsFile, rr: &mut RecordReader) -> Option<NtfsFileName> {
    let attr = base.get_attribute(NtfsAttributeType::AttributeList)?;
    if attr.header.is_non_resident != 0 {
        return None; // rare; counted as skipped
    }
    let header = attr.resident_header()?;
    let data = attr.data();
    let start = header.value_offset as usize;
    let end = start.checked_add(header.value_length as usize)?;
    if end > data.len() {
        return None;
    }
    let list = &data[start..end];

    let mut best: Option<NtfsFileName> = None;
    let mut off = 0usize;
    while off + size_of::<NtfsAttributeListEntry>() <= list.len() {
        let entry = unsafe { &*(list.as_ptr().add(off) as *const NtfsAttributeListEntry) };
        let len = entry.length as usize;
        if len < size_of::<NtfsAttributeListEntry>() || off + len > list.len() {
            break;
        }
        if entry.type_id == NtfsAttributeType::FileName as u32 {
            let target = entry.reference();
            if target != base.number
                && let Some(bytes) = rr.read_record(target)
            {
                let f = NtfsFile::new(target, bytes);
                if let Some(name) = pick_name(&f) {
                    let ns = name.header.namespace;
                    if ns == NtfsFileNamespace::Win32 as u8
                        || ns == NtfsFileNamespace::Win32AndDos as u8
                    {
                        return Some(name);
                    }
                    if best.is_none() {
                        best = Some(name);
                    }
                }
            }
        }
        off += len.next_multiple_of(8);
    }
    best
}

/// $STANDARD_INFORMATION + $DATA extract shared by every parse path.
#[derive(Default, Clone, Copy)]
struct RecordAttrs {
    size: u64,
    mtime: i64,
    is_reparse: bool,
    is_hidden: bool,
    is_system: bool,
}

fn extract_attrs(f: &NtfsFile) -> RecordAttrs {
    let mut a = RecordAttrs::default();
    f.attributes(|att| {
        if att.header.type_id == NtfsAttributeType::StandardInformation as u32 {
            if let Some(si) = att.as_standard_info() {
                a.mtime = si.modification_time as i64;
                a.is_reparse = si.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
                a.is_hidden = si.file_attributes & FILE_ATTRIBUTE_HIDDEN != 0;
                a.is_system = si.file_attributes & FILE_ATTRIBUTE_SYSTEM != 0;
            }
        } else if att.header.type_id == NtfsAttributeType::Data as u32 {
            if att.header.is_non_resident == 0 {
                if let Some(h) = att.resident_header() {
                    a.size = h.value_length as u64;
                }
            } else if let Some(h) = att.nonresident_header() {
                a.size = h.data_size;
            }
        }
    });
    a
}

// ── Parallel chunk parsing ───────────────────────────────────────────────

/// One entry parsed by a worker; the name lives in its batch's pools.
struct ParsedMeta {
    record: u64,
    parent_record: u64,
    frn: u64,
    name_off: u32,
    name_len: u32,
    is_dir: bool,
    attrs: RecordAttrs,
}

/// One worker's output for a record sub-range, in record order.
#[derive(Default)]
struct ParsedBatch {
    metas: Vec<ParsedMeta>,
    name_pool: Vec<u8>,
    lower_pool: Vec<u8>,
    deferred: Vec<(u64, Box<[u8]>)>,
    files: u64,
    dirs: u64,
    corrupt_records: u64,
    extension_records: u64,
    skipped_no_name: u64,
    deferred_unresolved: u64,
}

impl ParsedBatch {
    /// Encode a named record into this batch (WTF-8 pair + meta).
    fn push_named(&mut self, f: &NtfsFile, name: &NtfsFileName) {
        let name_data = name.data;
        let units = name.header.name_length as usize;
        let a = extract_attrs(f);
        if f.is_directory() {
            self.dirs += 1;
        } else {
            self.files += 1;
        }
        let name_off = self.name_pool.len() as u32;
        wtf8::push_wtf8_pair(
            &name_data[..units],
            &mut self.name_pool,
            &mut self.lower_pool,
        );
        self.metas.push(ParsedMeta {
            record: f.number,
            parent_record: name.header.parent_directory_reference,
            frn: f.reference_number(),
            name_off,
            name_len: self.name_pool.len() as u32 - name_off,
            is_dir: f.is_directory(),
            attrs: a,
        });
    }
}

/// Validate, fix up and parse every record in `bytes` (a record-aligned
/// slice whose first byte sits at `first_logical` in the $MFT stream).
/// Mirrors the sequential loop exactly — same skip conditions, same counts.
fn parse_subrange(bytes: &mut [u8], first_logical: u64, record_size: usize) -> ParsedBatch {
    let mut out = ParsedBatch::default();
    for off in (0..bytes.len()).step_by(record_size) {
        let number = (first_logical + off as u64) / record_size as u64;
        if number < FIRST_NORMAL_RECORD {
            continue; // metafiles; the builder seeds the root itself
        }
        let rec = &mut bytes[off..off + record_size];
        if !NtfsFile::is_valid(rec) {
            continue;
        }
        if !apply_fixup(rec) {
            out.corrupt_records += 1;
            continue;
        }
        let f = NtfsFile::new(number, rec);
        if !f.is_used() {
            continue;
        }
        if { f.header.base_reference } & 0x0000_FFFF_FFFF_FFFF != 0 {
            out.extension_records += 1;
            continue;
        }

        let Some(name) = pick_name(&f) else {
            if f.get_attribute(NtfsAttributeType::AttributeList).is_some() {
                out.deferred.push((number, rec.to_vec().into_boxed_slice()));
            } else {
                out.skipped_no_name += 1;
            }
            continue;
        };
        out.push_named(&f, &name);
    }
    out
}

/// Resolve deferred $ATTRIBUTE_LIST names in parallel, so the random
/// single-record reads — the slowest part of a real-volume scan once
/// streaming overlaps — issue at queue depth instead of one at a time.
/// Volume handles are pooled per rayon thread and opened lazily: opening
/// `\\.\C:` goes through every filesystem filter driver and costs tens of
/// ms — a per-chunk open measured 5× slower than the serial pass it was
/// meant to replace. Chunk order is preserved, so EntryId assignment
/// matches a serial loop.
fn resolve_deferred(
    volume_path: &str,
    runmap: &RunMap,
    record_size: usize,
    deferred: &[(u64, Box<[u8]>)],
) -> Vec<ParsedBatch> {
    use rayon::prelude::*;
    const DEFER_CHUNK: usize = 256;

    let readers: Vec<parking_lot::Mutex<Option<RecordReader>>> = (0..rayon::current_num_threads()
        .max(1))
        .map(|_| parking_lot::Mutex::new(None))
        .collect();

    deferred
        .par_chunks(DEFER_CHUNK)
        .map(|chunk| {
            let mut out = ParsedBatch::default();
            let slot = &readers[rayon::current_thread_index().unwrap_or(0) % readers.len()];
            let mut guard = slot.lock(); // uncontended: one slot per thread
            if guard.is_none() {
                match open_raw_volume(volume_path) {
                    Ok(file) => {
                        *guard = Some(RecordReader {
                            file,
                            map: runmap,
                            record_size,
                            buf: Vec::new(),
                        });
                    }
                    Err(e) => {
                        // Degraded but loud: these names stay unresolved and
                        // are counted like any other resolution failure.
                        tracing::warn!(
                            error = %e,
                            lost = chunk.len(),
                            "volume handle for deferred name pass unavailable"
                        );
                        out.deferred_unresolved += chunk.len() as u64;
                        return out;
                    }
                }
            }
            let rr = guard.as_mut().expect("reader installed above");
            for (number, bytes) in chunk {
                let f = NtfsFile::new(*number, bytes);
                match resolve_attr_list_name(&f, rr) {
                    Some(name) => out.push_named(&f, &name),
                    None => out.deferred_unresolved += 1,
                }
            }
            out
        })
        .collect()
}

/// Fan a chunk's record sub-ranges across rayon workers. The returned
/// batches are in sub-range order, so appending them sequentially yields
/// the same EntryId assignment as a fully sequential parse.
fn parse_chunk(chunk: &mut [u8], chunk_logical: u64, record_size: usize) -> Vec<ParsedBatch> {
    use rayon::prelude::*;
    let sub = (PARSE_SUB / record_size * record_size).max(record_size);
    chunk
        .par_chunks_mut(sub)
        .enumerate()
        .map(|(i, bytes)| parse_subrange(bytes, chunk_logical + (i * sub) as u64, record_size))
        .collect()
}

fn append_batches(
    b: &mut VolumeIndexBuilder,
    stats: &mut ScanStats,
    deferred: &mut Vec<(u64, Box<[u8]>)>,
    batches: Vec<ParsedBatch>,
) {
    for batch in batches {
        for m in &batch.metas {
            let range = m.name_off as usize..(m.name_off + m.name_len) as usize;
            b.push_encoded(EncodedEntry {
                record: m.record,
                parent_record: m.parent_record,
                frn: m.frn,
                name_wtf8: &batch.name_pool[range.clone()],
                lower_wtf8: &batch.lower_pool[range],
                is_dir: m.is_dir,
                is_reparse: m.attrs.is_reparse,
                is_hidden: m.attrs.is_hidden,
                is_system: m.attrs.is_system,
                size: m.attrs.size,
                mtime: m.attrs.mtime,
            });
        }
        stats.files += batch.files;
        stats.dirs += batch.dirs;
        stats.corrupt_records += batch.corrupt_records;
        stats.extension_records += batch.extension_records;
        stats.skipped_no_name += batch.skipped_no_name;
        stats.deferred_unresolved += batch.deferred_unresolved;
        deferred.extend(batch.deferred);
    }
}

// ── Read-ahead pipeline ──────────────────────────────────────────────────

/// Record-aligned read unit of the $MFT data stream.
struct Chunk {
    logical: u64,
    phys: u64,
    want: usize,
}

/// Pure arithmetic version of the old read loop: same chunking, same
/// sparse-hole skipping, no I/O.
fn plan_chunks(map: &RunMap, data_size: u64, record_size: usize) -> Vec<Chunk> {
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
fn run_chunk_pipeline(
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

/// Full initial scan: stream the volume's $MFT and build the in-memory
/// index. `drive` is a drive letter spec like `C:`.
pub fn scan_volume(drive: &str) -> Result<(VolumeIndex, ScanStats), MftError> {
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
    let record_size = volume.file_record_size as usize;

    // Bootstrap: record 0 → the $MFT's own data runs.
    let (data_size, runmap) = {
        let mut reader =
            ntfs_reader::aligned_reader::open_volume(std::path::Path::new(&volume_path))
                .map_err(NtfsReaderError::from)?;
        let rec0 = Mft::get_record_fs(&mut reader, volume.file_record_size, volume.mft_position)?;
        let f0 = NtfsFile::new(0, &rec0);
        let data_attr = f0
            .get_attribute(NtfsAttributeType::Data)
            .ok_or_else(|| NtfsReaderError::MissingMftAttribute("Data".to_string()))?;
        let (size, runs) = data_attr.get_nonresident_data_runs(&volume)?;
        (size, RunMap::from_data_runs(&runs))
    };
    stats.mft_bytes = data_size;

    let chunks = plan_chunks(&runmap, data_size, record_size);
    let mut b = VolumeIndexBuilder::new(drive, ROOT_RECORD);
    let mut deferred: Vec<(u64, Box<[u8]>)> = Vec::new();
    let mut parse_time = Duration::ZERO;

    let (read_time, fallbacks) = run_chunk_pipeline(&volume_path, &chunks, &mut |i, bytes| {
        let t = Instant::now();
        let batches = parse_chunk(bytes, chunks[i].logical, record_size);
        append_batches(&mut b, &mut stats, &mut deferred, batches);
        parse_time += t.elapsed();
    })
    .map_err(MftError::Ntfs)?;
    stats.elapsed_mft_load_ms = read_time.as_millis() as u64;
    stats.elapsed_parse_ms = parse_time.as_millis() as u64;
    stats.pipeline_fallbacks = fallbacks;

    // Deferred pass: names hiding behind $ATTRIBUTE_LIST (~tens of
    // thousands on a real C:) resolved with targeted single-record reads —
    // in parallel: measured on a real 1.27M-entry C:, the serial version of
    // this pass cost more than the streaming read it followed.
    let t_deferred = Instant::now();
    stats.deferred_names = deferred.len() as u64;
    let batches = resolve_deferred(&volume_path, &runmap, record_size, &deferred);
    append_batches(&mut b, &mut stats, &mut Vec::new(), batches);
    stats.elapsed_deferred_ms = t_deferred.elapsed().as_millis() as u64;

    // Degradations are normal in small numbers; make them visible either way.
    if stats.corrupt_records > 0 {
        tracing::warn!(volume = %drive, count = stats.corrupt_records, "corrupt MFT records skipped");
    }
    if stats.deferred_unresolved > 0 {
        tracing::warn!(
            volume = %drive,
            count = stats.deferred_unresolved,
            "attribute-list names unresolved"
        );
    }

    let (idx, finish) = b.finish_timed();
    stats.elapsed_build_ms = finish.build_ms;
    stats.elapsed_sort_ms = finish.sort_ms;
    stats.elapsed_total_ms = t0.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = peak_working_set();
    Ok((idx, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// plan_chunks must reproduce the old read loop's arithmetic: record
    /// alignment, sparse-hole skipping, full logical coverage in order.
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
        let dir = std::env::temp_dir().join(format!("fmf-scan-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_propagates_read_errors() {
        let dir = std::env::temp_dir().join(format!("fmf-scan-pipe-err-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Equivalence gate against the whole-load reference path. Run from an
    /// elevated shell: FMF_ADMIN_TESTS=1 cargo test -- --ignored streaming
    /// The volume is live, so a small drift tolerance is allowed.
    #[test]
    #[ignore]
    fn streaming_scan_matches_reference() {
        if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
            eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
            return;
        }
        let (new_idx, new_stats) = scan_volume("C:").expect("streaming scan");
        let (old_idx, old_stats) = crate::mft::scan_volume_reference("C:").expect("reference");

        let drift = (new_idx.len() as i64 - old_idx.len() as i64).unsigned_abs();
        assert!(
            drift < old_idx.len() as u64 / 500,
            "entry counts diverged: streaming {} vs reference {} (files {}/{} dirs {}/{})",
            new_idx.len(),
            old_idx.len(),
            new_stats.files,
            old_stats.files,
            new_stats.dirs,
            old_stats.dirs,
        );

        // Sampled records must agree on name and size where both saw them.
        // Reparse points are excluded: pick_name keeps their names on
        // purpose while the reference's get_best_file_name skips them, so
        // the two resolvers legitimately disagree there (and on this class
        // only — see the module docs of `pick_name`).
        let mut checked = 0u64;
        let mut matched = 0u64;
        let mut size_matched = 0u64;
        for sample in (0..old_idx.len() as u32).step_by(997) {
            let old_rec = crate::index::masked(old_idx.frn(sample));
            let (Some(o), Some(n)) = (
                old_idx.entry_by_record(old_rec),
                new_idx.entry_by_record(old_rec),
            ) else {
                continue;
            };
            if old_idx.is_reparse(o) || new_idx.is_reparse(n) {
                continue;
            }
            checked += 1;
            if old_idx.name(o) == new_idx.name(n) {
                matched += 1;
            }
            if old_idx.size(o) == new_idx.size(n) {
                size_matched += 1;
            }
        }
        assert!(checked > 100, "sample too small: {checked}");
        assert!(
            matched as f64 / checked as f64 > 0.999,
            "sampled name mismatch: {matched}/{checked}"
        );
        // Sizes drift legitimately: the volume is live and the two scans run
        // a minute apart, so actively-written files differ. Names only move
        // on renames — hence the looser size bar.
        assert!(
            size_matched as f64 / checked as f64 > 0.99,
            "sampled size mismatch: {size_matched}/{checked}"
        );
    }
}
