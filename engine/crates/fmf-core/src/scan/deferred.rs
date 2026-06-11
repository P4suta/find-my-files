//! Deferred $ATTRIBUTE_LIST name resolution (ADR-0011): name-bearing
//! extension records are cached in RAM while the $MFT streams through, so
//! this pass resolves names without disk reads; anything missing (cache
//! cap, torn records) falls back to a targeted read of the live volume.

use ntfs_reader::api::{
    NtfsAttributeListEntry, NtfsAttributeType, NtfsFileName, NtfsFileNamespace,
};
use ntfs_reader::file::NtfsFile;
use rustc_hash::FxHashMap;

use crate::mft::pick_name;

use super::parse::{ParsedBatch, RecordArena};
use super::volume_io::{RunMap, apply_fixup, open_raw_volume};

/// Upper bound on cached name-bearing extension records (~1KiB each, so
/// ≤128MiB transient). A real C: has tens of thousands; past the cap the
/// deferred pass falls back to disk reads for the remainder.
pub(super) const EXT_NAME_CACHE_CAP: usize = 128 << 10;

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

/// Disk fallback for extension records missing from the streamed cache —
/// opened only when actually needed (expected: never on a healthy scan).
struct LazyRecordReader<'a> {
    volume_path: &'a str,
    map: &'a RunMap,
    record_size: usize,
    inner: Option<RecordReader<'a>>,
    failed: bool,
    /// Failed `read_record` calls — each one is a name that stays
    /// unresolved until the next rescan. `resolve_deferred` folds this into
    /// its batch, so the count reaches `ScanStats` (黙らない).
    failures: u64,
}

impl<'a> LazyRecordReader<'a> {
    fn new(volume_path: &'a str, map: &'a RunMap, record_size: usize) -> Self {
        LazyRecordReader {
            volume_path,
            map,
            record_size,
            inner: None,
            failed: false,
            failures: 0,
        }
    }

    fn read_record(&mut self, number: u64) -> Option<&[u8]> {
        if self.inner.is_none() && !self.failed {
            match open_raw_volume(self.volume_path) {
                Ok(file) => {
                    self.inner = Some(RecordReader {
                        file,
                        map: self.map,
                        record_size: self.record_size,
                        buf: Vec::new(),
                    });
                }
                Err(e) => {
                    self.failed = true;
                    tracing::warn!(error = %e, "deferred-pass fallback volume handle unavailable");
                }
            }
        }
        let Some(inner) = self.inner.as_mut() else {
            self.failures += 1;
            return None;
        };
        let got = inner.read_record(number);
        if got.is_none() {
            self.failures += 1;
        }
        got
    }
}

/// Resolve the display name of a record whose $FILE_NAME lives in extension
/// records (resident $ATTRIBUTE_LIST → referenced records). Targets come
/// from the streamed extension-record cache; anything missing (cache cap,
/// torn records) falls back to a targeted disk read. Mirrors ntfs-reader's
/// get_best_file_name without needing the whole MFT in RAM.
fn resolve_attr_list_name(
    base: &NtfsFile,
    ext: &FxHashMap<u64, u32>,
    arena: &RecordArena,
    rr: &mut LazyRecordReader,
) -> Option<NtfsFileName> {
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
            if target != base.number {
                let picked = match ext.get(&target) {
                    Some(&slot) => pick_name(&NtfsFile::new(target, arena.get(slot))),
                    None => rr
                        .read_record(target)
                        .and_then(|bytes| pick_name(&NtfsFile::new(target, bytes))),
                };
                if let Some(name) = picked {
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

/// Resolve deferred $ATTRIBUTE_LIST names in parallel — almost entirely
/// from RAM: every target is an extension record and the whole $MFT just
/// streamed through the pipeline, so `ext` already holds the bytes
/// (ADR-0011). Chunk order is preserved, so EntryId assignment matches a
/// serial loop.
pub(super) fn resolve_deferred(
    volume_path: &str,
    runmap: &RunMap,
    record_size: usize,
    ext: &FxHashMap<u64, u32>,
    arena: &RecordArena,
    deferred: &[(u64, u32)],
) -> Vec<ParsedBatch> {
    use rayon::prelude::*;
    const DEFER_CHUNK: usize = 256;

    deferred
        .par_chunks(DEFER_CHUNK)
        .map(|chunk| {
            let mut out = ParsedBatch::default();
            let mut rr = LazyRecordReader::new(volume_path, runmap, record_size);
            for &(number, slot) in chunk {
                let f = NtfsFile::new(number, arena.get(slot));
                match resolve_attr_list_name(&f, ext, arena, &mut rr) {
                    Some(name) => out.push_named(&f, &name),
                    None => out.deferred_unresolved += 1,
                }
            }
            out.deferred_name_read_failures = rr.failures;
            out
        })
        .collect()
}
