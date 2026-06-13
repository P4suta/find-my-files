//! Parallel chunk parsing (ADR-0011): record sub-ranges of one chunk fan
//! out across rayon workers, each producing a [`ParsedBatch`]; the builder
//! appends the batches in chunk order, so `EntryId` assignment is
//! deterministic.

use ntfs_reader::api::{FIRST_NORMAL_RECORD, NtfsAttributeType, NtfsFileName};
use ntfs_reader::file::NtfsFile;
use rustc_hash::FxHashMap;

use crate::index::{EncodedEntry, VolumeIndexBuilder};
use crate::mft::pick_name;
use crate::wtf8;

use super::ScanStats;
use super::deferred::EXT_NAME_CACHE_CAP;
use super::volume_io::apply_fixup;

/// Sub-range fed to one parse worker — small enough to spread a 16MiB chunk
/// across cores, large enough to amortize the per-task overhead.
const PARSE_SUB: usize = 1 << 20;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Fixed-size record store for the deferred/extension caches: records live
/// back-to-back in one growable allocation, addressed by slot (ADR-0012).
pub(super) struct RecordArena {
    data: Vec<u8>,
    record_size: usize,
}

impl RecordArena {
    pub(super) const fn new(record_size: usize) -> Self {
        Self {
            data: Vec::new(),
            record_size,
        }
    }

    fn push(&mut self, rec: &[u8]) -> u32 {
        debug_assert_eq!(rec.len(), self.record_size);
        let slot = (self.data.len() / self.record_size) as u32;
        self.data.extend_from_slice(rec);
        slot
    }

    pub(super) fn get(&self, slot: u32) -> &[u8] {
        let off = slot as usize * self.record_size;
        &self.data[off..off + self.record_size]
    }
}

/// $`STANDARD_INFORMATION` + $DATA extract shared by every parse path.
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
pub(super) struct ParsedBatch {
    metas: Vec<ParsedMeta>,
    name_pool: Vec<u8>,
    lower_pool: Vec<u8>,
    /// Raw record bytes referenced by `deferred`/`extensions` — one pool
    /// per batch instead of a box per record (the global `RecordArena` gets
    /// them at append time).
    rec_pool: Vec<u8>,
    deferred: Vec<(u64, std::ops::Range<usize>)>,
    /// Extension records carrying a $`FILE_NAME` — the targets the deferred
    /// pass will need. Keeping them now turns that pass's random disk reads
    /// into RAM lookups (the whole $MFT just streamed through here anyway).
    extensions: Vec<(u64, std::ops::Range<usize>)>,
    files: u64,
    dirs: u64,
    corrupt_records: u64,
    extension_records: u64,
    skipped_no_name: u64,
    pub(super) deferred_unresolved: u64,
    /// Deferred-pass disk reads that failed (`LazyRecordReader`) — folded
    /// into `ScanStats::deferred_name_read_failures` at append time.
    pub(super) deferred_name_read_failures: u64,
}

impl ParsedBatch {
    fn push_record(&mut self, bytes: &[u8]) -> std::ops::Range<usize> {
        let start = self.rec_pool.len();
        self.rec_pool.extend_from_slice(bytes);
        start..self.rec_pool.len()
    }

    /// Encode a named record into this batch (WTF-8 pair + meta).
    pub(super) fn push_named(&mut self, f: &NtfsFile, name: &NtfsFileName) {
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
            if f.get_attribute(NtfsAttributeType::FileName).is_some() {
                let range = out.push_record(rec);
                out.extensions.push((number, range));
            }
            continue;
        }

        let Some(name) = pick_name(&f) else {
            if f.get_attribute(NtfsAttributeType::AttributeList).is_some() {
                let range = out.push_record(rec);
                out.deferred.push((number, range));
            } else {
                out.skipped_no_name += 1;
            }
            continue;
        };
        out.push_named(&f, &name);
    }
    out
}

/// Fan a chunk's record sub-ranges across rayon workers. The returned
/// batches are in sub-range order, so appending them sequentially yields
/// the same `EntryId` assignment as a fully sequential parse.
pub(super) fn parse_chunk(
    chunk: &mut [u8],
    chunk_logical: u64,
    record_size: usize,
) -> Vec<ParsedBatch> {
    use rayon::prelude::*;
    let sub = (PARSE_SUB / record_size * record_size).max(record_size);
    chunk
        .par_chunks_mut(sub)
        .enumerate()
        .map(|(i, bytes)| parse_subrange(bytes, chunk_logical + (i * sub) as u64, record_size))
        .collect()
}

pub(super) fn append_batches(
    b: &mut VolumeIndexBuilder,
    stats: &mut ScanStats,
    deferred: &mut Vec<(u64, u32)>,
    extensions: &mut FxHashMap<u64, u32>,
    arena: &mut RecordArena,
    batches: Vec<ParsedBatch>,
) {
    for batch in batches {
        for (number, range) in batch.extensions {
            if extensions.len() < EXT_NAME_CACHE_CAP {
                extensions.insert(number, arena.push(&batch.rec_pool[range]));
            } else {
                stats.ext_name_cache_skipped += 1;
            }
        }
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
        stats.deferred_name_read_failures += batch.deferred_name_read_failures;
        for (number, range) in batch.deferred {
            deferred.push((number, arena.push(&batch.rec_pool[range])));
        }
    }
}
