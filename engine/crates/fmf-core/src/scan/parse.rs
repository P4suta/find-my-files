//! Parallel chunk parsing (ADR-0011): record sub-ranges of one chunk fan
//! out across rayon workers, each producing a [`ParsedBatch`]; the builder
//! appends the batches in chunk order, so `EntryId` assignment is
//! deterministic.

use rustc_hash::FxHashMap;

use crate::index::{EncodedEntry, Frn, VolumeIndexBuilder};
use crate::mft::collect_searchable_names;
use crate::wtf8;

use super::ScanStats;
use super::deferred::DEFERRED_RECORD_ARENA_MAX_BYTES;
use crate::ondisk::fixup::apply_fixup;
use crate::ondisk::ntfs::{
    EXTEND_RECORD, FIRST_NORMAL_RECORD, NtfsAttribute, NtfsAttributeType, NtfsFile, NtfsFileName,
};
use crate::ondisk::record::attributes_complete;

/// Sub-range fed to one parse worker — small enough to spread a 16MiB chunk
/// across cores, large enough to amortize the per-task overhead.
const PARSE_SUB: usize = 1 << 20;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Size of the unnamed `$DATA` stream — the file size a user recognizes.
///
/// Named `$DATA` attributes (alternate data streams) are deliberately skipped
/// rather than summed or indexed as separate rows: the index holds one
/// searchable row per directory link (ADR-0001, ADR-0005), and an ADS is
/// neither separately named in a directory nor visible in Explorer, so
/// surfacing one would produce results the user cannot act on.
fn unnamed_data_size(attribute: &NtfsAttribute<'_>) -> Option<u64> {
    if attribute.header.is_non_resident == 0 {
        attribute.resident_value_length().map(u64::from)
    } else {
        attribute
            .nonresident_header()
            .map(|header| header.data_size)
    }
}

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

    fn try_push_bounded(&mut self, rec: &[u8], max_bytes: usize) -> Option<u32> {
        debug_assert_eq!(rec.len(), self.record_size);
        let end = self.data.len().checked_add(rec.len())?;
        if end > max_bytes {
            return None;
        }
        let slot = u32::try_from(self.data.len() / self.record_size).ok()?;
        self.data.extend_from_slice(rec);
        Some(slot)
    }

    pub(super) fn get(&self, slot: u32) -> &[u8] {
        let off = slot as usize * self.record_size;
        &self.data[off..off + self.record_size]
    }
}

/// $`STANDARD_INFORMATION` + unnamed $DATA extract shared by every parse path.
#[derive(Clone, Copy)]
pub(super) struct RecordAttrs {
    size: u64,
    mtime: i64,
    is_reparse: bool,
    is_hidden: bool,
    is_system: bool,
}

impl RecordAttrs {
    pub(super) const fn with_stat(mut self, size: u64, mtime: i64) -> Self {
        self.size = size;
        self.mtime = mtime;
        self
    }
}

pub(super) fn extract_attrs(f: &NtfsFile) -> Option<RecordAttrs> {
    let mut size = None;
    let mut standard_information = None;
    let mut attribute_lists = 0usize;
    let mut valid = true;
    f.attributes(|att| {
        if att.header.type_id == NtfsAttributeType::StandardInformation as u32 {
            if att.header.name_length != 0
                || att.header.flags != 0
                || att.header.is_non_resident != 0
                || standard_information.is_some()
            {
                valid = false;
                return;
            }
            standard_information = att.as_standard_info();
            valid &= standard_information.is_some();
        } else if att.header.type_id == NtfsAttributeType::AttributeList as u32 {
            attribute_lists += 1;
            if att.header.name_length != 0 || att.header.flags != 0 || attribute_lists != 1 {
                valid = false;
            }
        } else if att.header.type_id == NtfsAttributeType::Data as u32
            && att.header.name_length == 0
        {
            if size.is_some() {
                valid = false;
                return;
            }
            size = unnamed_data_size(att);
            valid &= size.is_some();
        }
    });
    let si = standard_information?;
    if !valid {
        return None;
    }
    // A missing (or surprising) unnamed $DATA is *not* record corruption, and
    // must never reach `corrupt_records` — that counter aborts the whole
    // volume with `MftError::CorruptRecords`. Real NTFS breaks both halves of
    // the naive "files have exactly one unnamed $DATA, directories have none"
    // rule: `\$Extend\$UsnJrnl` carries only the named `$Max`/`$J` streams,
    // has no directory flag, and lives past `FIRST_NORMAL_RECORD`, so it is
    // present on every volume this engine indexes; `$Quota`/`$ObjId`/
    // `$Reparse` are index-only in the same way. Such a record is still
    // perfectly nameable — only its size is unknown, so publish it with size
    // 0 instead of refusing to publish the index. Directories report 0 for
    // the same reason: a stray unnamed $DATA on a directory is not the size a
    // user recognizes. Structural checks above (duplicate/named/flagged
    // $STANDARD_INFORMATION, duplicate unnamed $DATA, duplicate
    // $ATTRIBUTE_LIST, undecodable values) stay strict.
    let size = if f.is_directory() { None } else { size };
    Some(RecordAttrs {
        size: size.unwrap_or(0),
        mtime: si.modification_time as i64,
        is_reparse: si.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        is_hidden: si.file_attributes & FILE_ATTRIBUTE_HIDDEN != 0,
        is_system: si.file_attributes & FILE_ATTRIBUTE_SYSTEM != 0,
    })
}

/// One entry parsed by a worker; the name lives in its batch's pools.
struct ParsedMeta {
    /// Raw OS values (the parse layer stays in `u64`); wrapped into [`Frn`]
    /// when the entry crosses into the index in `append_batches`.
    parent_frn: u64,
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
    /// Extension records carrying a $`FILE_NAME` or an $`ATTRIBUTE_LIST`
    /// extent — the targets the deferred pass will need. Keeping them now
    /// turns that pass's random disk reads into RAM lookups.
    extensions: Vec<(u64, std::ops::Range<usize>)>,
    files: u64,
    dirs: u64,
    corrupt_records: u64,
    extension_records: u64,
    skipped_no_name: u64,
    /// Deferred-pass disk reads that failed (`LazyRecordReader`) — folded
    /// into `ScanStats::deferred_name_read_failures` at append time.
    pub(super) deferred_name_read_failures: u64,
    /// Deferred-pass objects the live source could not size — folded into
    /// `ScanStats::deferred_stat_failures` at append time.
    pub(super) deferred_stat_failures: u64,
}

impl ParsedBatch {
    fn push_record(&mut self, bytes: &[u8]) -> std::ops::Range<usize> {
        let start = self.rec_pool.len();
        self.rec_pool.extend_from_slice(bytes);
        start..self.rec_pool.len()
    }

    /// Encode a named record into this batch (WTF-8 pair + meta).
    pub(super) fn push_named(
        &mut self,
        f: &NtfsFile,
        name: &NtfsFileName<'_>,
        attrs: RecordAttrs,
    ) -> bool {
        self.push_utf16le_link_with_attrs(
            f,
            name.header.parent_directory_reference,
            name.utf16le,
            attrs,
        )
    }

    pub(super) fn push_utf16le_link_with_attrs(
        &mut self,
        f: &NtfsFile,
        parent_frn: u64,
        name: &[u8],
        attrs: RecordAttrs,
    ) -> bool {
        self.push_encoded(f, parent_frn, attrs, |original, folded| {
            wtf8::push_wtf8le_pair(name, original, folded)
        })
    }

    pub(super) fn push_link(
        &mut self,
        f: &NtfsFile,
        parent_frn: u64,
        name: &[u16],
        attrs: RecordAttrs,
    ) -> bool {
        self.push_encoded(f, parent_frn, attrs, |original, folded| {
            wtf8::push_wtf8_pair(name, original, folded);
            true
        })
    }

    fn push_encoded(
        &mut self,
        f: &NtfsFile,
        parent_frn: u64,
        a: RecordAttrs,
        encode: impl FnOnce(&mut Vec<u8>, &mut Vec<u8>) -> bool,
    ) -> bool {
        let name_before = self.name_pool.len();
        let lower_before = self.lower_pool.len();
        if !encode(&mut self.name_pool, &mut self.lower_pool) {
            self.name_pool.truncate(name_before);
            self.lower_pool.truncate(lower_before);
            return false;
        }
        let is_dir = f.is_directory();
        if is_dir {
            self.dirs += 1;
        } else {
            self.files += 1;
        }
        let name_off = name_before as u32;
        debug_assert_eq!(
            self.name_pool.len() - name_off as usize,
            self.lower_pool.len() - lower_before
        );
        self.metas.push(ParsedMeta {
            parent_frn,
            frn: f.reference_number(),
            name_off,
            name_len: self.name_pool.len() as u32 - name_off,
            is_dir,
            attrs: a,
        });
        true
    }
}

fn push_searchable_names(out: &mut ParsedBatch, file: &NtfsFile<'_>) -> Option<usize> {
    let attrs = extract_attrs(file)?;
    let names = collect_searchable_names(file)?;
    let count = names.len();
    for name in names {
        if !out.push_named(file, &name, attrs) {
            return None;
        }
    }
    Some(count)
}

/// Validate, fix up and parse every record in `bytes` (a record-aligned
/// slice whose first byte sits at `first_logical` in the $MFT stream).
/// Mirrors the sequential loop exactly — same skip conditions, same counts.
fn parse_subrange(
    bytes: &mut [u8],
    first_logical: u64,
    record_size: usize,
    sector_size: usize,
) -> ParsedBatch {
    let mut out = ParsedBatch::default();
    for off in (0..bytes.len()).step_by(record_size) {
        let number = (first_logical + off as u64) / record_size as u64;
        // Metafiles; the builder seeds the root itself. `\$Extend` is the one
        // exception: it is a directory whose children sit above the threshold
        // and are indexed, so skipping it would publish rows whose exact parent
        // resolves to nothing — and the builder turns one unresolved parent
        // into a whole-volume failure. It is also transitive: `$RmMetadata` is
        // itself a directory, so dropping `\$Extend` orphans its grandchildren
        // too. Indexing `\$Extend` lets the whole subtree resolve to the root.
        if number < FIRST_NORMAL_RECORD && number != EXTEND_RECORD {
            continue;
        }
        let rec = &mut bytes[off..off + record_size];
        if !NtfsFile::is_valid(rec, sector_size) {
            // NTFS can leave preallocated, never-used MFT slots zeroed. Those
            // are not records and are safe to ignore. Any non-zero slot with
            // an invalid signature, however, may be an allocated record whose
            // name would otherwise disappear from the published index.
            if rec.iter().any(|&byte| byte != 0) {
                out.corrupt_records += 1;
            }
            continue;
        }
        if !apply_fixup(rec, sector_size) {
            out.corrupt_records += 1;
            continue;
        }
        if !attributes_complete(rec) {
            out.corrupt_records += 1;
            continue;
        }
        let Some(f) = NtfsFile::parse(number, rec, sector_size) else {
            out.corrupt_records += 1;
            continue;
        };
        if !f.is_used() {
            continue;
        }
        if f.header.base_reference != 0 {
            out.extension_records += 1;
            if f.get_attribute(NtfsAttributeType::FileName).is_some()
                || f.get_attribute(NtfsAttributeType::AttributeList).is_some()
            {
                let range = out.push_record(rec);
                out.extensions.push((number, range));
            }
            continue;
        }

        if f.get_attribute(NtfsAttributeType::AttributeList).is_some() {
            // An attribute list can move additional hard-link FILE_NAME
            // attributes into extension records. Defer the whole object so
            // its link set is emitted atomically and completely.
            let range = out.push_record(rec);
            out.deferred.push((f.reference_number(), range));
            continue;
        }
        match push_searchable_names(&mut out, &f) {
            Some(0) => out.skipped_no_name += 1,
            Some(_) => {}
            None => out.corrupt_records += 1,
        }
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
    sector_size: usize,
) -> Vec<ParsedBatch> {
    use rayon::prelude::*;
    let sub = (PARSE_SUB / record_size * record_size).max(record_size);
    chunk
        .par_chunks_mut(sub)
        .enumerate()
        .map(|(i, bytes)| {
            parse_subrange(
                bytes,
                chunk_logical + (i * sub) as u64,
                record_size,
                sector_size,
            )
        })
        .collect()
}

pub(super) fn append_batches(
    b: &mut VolumeIndexBuilder,
    stats: &mut ScanStats,
    deferred: &mut Vec<(u64, Option<u32>)>,
    extensions: &mut FxHashMap<u64, u32>,
    arena: &mut RecordArena,
    batches: Vec<ParsedBatch>,
) -> u64 {
    append_batches_bounded(
        b,
        stats,
        deferred,
        extensions,
        arena,
        batches,
        DEFERRED_RECORD_ARENA_MAX_BYTES,
    )
}

fn append_batches_bounded(
    b: &mut VolumeIndexBuilder,
    stats: &mut ScanStats,
    deferred: &mut Vec<(u64, Option<u32>)>,
    extensions: &mut FxHashMap<u64, u32>,
    arena: &mut RecordArena,
    batches: Vec<ParsedBatch>,
    max_arena_bytes: usize,
) -> u64 {
    let mut corrupt_records = 0u64;
    for batch in batches {
        corrupt_records += batch.corrupt_records;
        for (number, range) in batch.extensions {
            match arena.try_push_bounded(&batch.rec_pool[range], max_arena_bytes) {
                Some(slot) => {
                    extensions.insert(number, slot);
                }
                None => stats.deferred_record_cache_spills += 1,
            }
        }
        for m in &batch.metas {
            let range = m.name_off as usize..(m.name_off + m.name_len) as usize;
            b.push_encoded(EncodedEntry {
                parent_frn: Frn(m.parent_frn),
                frn: Frn(m.frn),
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
        stats.extension_records += batch.extension_records;
        stats.skipped_no_name += batch.skipped_no_name;
        stats.deferred_name_read_failures += batch.deferred_name_read_failures;
        stats.deferred_stat_failures += batch.deferred_stat_failures;
        for (reference, range) in batch.deferred {
            let slot = arena.try_push_bounded(&batch.rec_pool[range], max_arena_bytes);
            if slot.is_none() {
                stats.deferred_record_cache_spills += 1;
            }
            deferred.push((reference, slot));
        }
    }
    corrupt_records
}

#[cfg(test)]
mod tests {
    //! Byte-fixture replay of the $MFT parse path — the analogue of
    //! `tests/usn_replay.rs` for the scan side. Synthetic NTFS `FILE` records
    //! are built byte-for-byte from the documented on-disk layout and run
    //! through the real `parse_subrange` / `parse_chunk` / `append_batches` — no OS, no
    //! elevation, no seam. This covers `scan/parse.rs`, which the MFT-scan
    //! privilege barrier otherwise leaves to elevated `FMF_ADMIN_TESTS` only.
    //!
    //! Layout references (all little-endian, from the NTFS on-disk grammar):
    //!   `NtfsFileRecordHeader` (42 B): signature[4] @0, `usa_offset` u16 @4,
    //!     `usa_length` u16 @6, lsn u64 @8, sequence u16 @16, `link_count` u16 @18,
    //!     `attrs_offset` u16 @20, flags u16 @22, `used_size` u32 @24,
    //!     `alloc_size` u32 @28, `base_reference` u64 @32, `next_attr_id` u16 @40.
    //!   `NtfsAttributeHeader` (16 B): `type_id` u32 @0, length u32 @4,
    //!     `non_resident` u8 @8, `name_length` u8 @9, `name_offset` u16 @10,
    //!     flags u16 @12, id u16 @14.
    //!   Resident value header adds: `value_length` u32 @16, `value_offset` u16 @20.
    //!   `NtfsStandardInformation`: `modification_time` u64 @8, attributes u32 @32.
    //!   `NtfsFileNameHeader` (66 B): `parent_ref` u64 @0, … `name_length` u8 @64,
    //!     namespace u8 @65; the UTF-16 name follows at @66.

    use super::*;

    const REC: usize = 1024;
    const SECTORS: usize = REC / 512;
    /// Update-sequence sentinel written at every sector tail; the fixup pass
    /// checks for it and restores the real (zero) bytes from the USA.
    const USN_SENTINEL: u16 = 0x0001;

    // NTFS attribute type ids.
    const T_STD_INFO: u32 = 0x10;
    const T_ATTR_LIST: u32 = 0x20;
    const T_FILE_NAME: u32 = 0x30;
    const T_DATA: u32 = 0x80;
    const T_END: u32 = 0xFFFF_FFFF;

    /// File reference number for a base record at sequence 1: the record number
    /// in the low 48 bits, the sequence in the top 16 (mirrors the scanner's own
    /// FRN packing). Keeps the record number a plain decimal instead of a hex
    /// literal buried in a bitwise OR.
    const fn frn(record: u64) -> u64 {
        (1u64 << 48) | record
    }

    // $STANDARD_INFORMATION file_attributes bits.
    const A_HIDDEN: u32 = 0x2;
    const A_SYSTEM: u32 = 0x4;
    const A_ARCHIVE: u32 = 0x20;
    const A_REPARSE: u32 = 0x400;

    // Namespaces (NtfsFileNamespace).
    const NS_POSIX: u8 = 0;
    const NS_WIN32: u8 = 1;
    const NS_DOS: u8 = 2;
    const NS_WIN32_DOS: u8 = 3;

    // File-record flags (NtfsFileFlags).
    const F_IN_USE: u16 = 0x1;
    const F_DIR: u16 = 0x2;

    fn put_u16(buf: &mut [u8], off: usize, v: u16) {
        buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u64(buf: &mut [u8], off: usize, v: u64) {
        buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// A resident attribute: 16-byte header + resident value header + value,
    /// length rounded up to the 8-byte boundary NTFS uses.
    fn resident_attr(type_id: u32, value: &[u8]) -> Vec<u8> {
        const VALUE_OFFSET: usize = 24;
        let length = (VALUE_OFFSET + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; length];
        put_u32(&mut a, 0, type_id);
        put_u32(&mut a, 4, length as u32);
        // is_non_resident=0, name_length=0 (already zero).
        put_u32(&mut a, 16, value.len() as u32); // value_length
        put_u16(&mut a, 20, VALUE_OFFSET as u16); // value_offset
        a[VALUE_OFFSET..VALUE_OFFSET + value.len()].copy_from_slice(value);
        a
    }

    fn attribute_id(mut attribute: Vec<u8>, id: u16) -> Vec<u8> {
        put_u16(&mut attribute, 14, id);
        attribute
    }

    fn attribute_list_entry(type_id: u32, target_reference: u64, id: u16) -> Vec<u8> {
        const HEADER: usize = 26;
        let mut entry = vec![0u8; HEADER.next_multiple_of(8)];
        let record_length = entry.len() as u16;
        put_u32(&mut entry, 0, type_id);
        put_u16(&mut entry, 4, record_length);
        put_u64(&mut entry, 16, target_reference);
        put_u16(&mut entry, 24, id);
        entry
    }

    /// A named resident attribute. Named $DATA is an alternate data stream,
    /// not the unnamed default stream whose length is the file size.
    fn named_resident_attr(type_id: u32, name: &[u16], value: &[u8]) -> Vec<u8> {
        const NAME_OFFSET: usize = 24;
        let value_offset = (NAME_OFFSET + name.len() * 2).next_multiple_of(8);
        let length = (value_offset + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; length];
        put_u32(&mut a, 0, type_id);
        put_u32(&mut a, 4, length as u32);
        a[9] = name.len() as u8;
        put_u16(&mut a, 10, NAME_OFFSET as u16);
        put_u32(&mut a, 16, value.len() as u32);
        put_u16(&mut a, 20, value_offset as u16);
        for (i, unit) in name.iter().enumerate() {
            put_u16(&mut a, NAME_OFFSET + i * 2, *unit);
        }
        a[value_offset..value_offset + value.len()].copy_from_slice(value);
        a
    }

    /// A non-resident $DATA attribute carrying only the geometry the parser
    /// reads (`data_size` @48) plus the mapping-pair region every real
    /// non-resident attribute owns.
    ///
    /// On disk the run list always follows the 64-byte non-resident header and
    /// always ends with the 0x00 terminator, so the smallest legal
    /// 8-byte-aligned attribute length is 72 — never 64, which would put
    /// `MappingPairsOffset` at the attribute's end with no room for the
    /// terminator. A single zero byte at @64 *is* that terminator (an empty,
    /// fully sparse-free run list), so the rest of the record stays as before.
    fn data_nonresident(data_size: u64) -> Vec<u8> {
        const HEADER: usize = 64; // size_of::<NtfsNonResidentAttributeHeader>()
        let length = (HEADER + 1).next_multiple_of(8); // header + run-list terminator
        let mut a = vec![0u8; length];
        put_u32(&mut a, 0, T_DATA);
        put_u32(&mut a, 4, length as u32);
        a[8] = 1; // is_non_resident
        put_u16(&mut a, 32, HEADER as u16); // data_runs_offset
        put_u64(&mut a, 48, data_size);
        a
    }

    fn std_info(mtime: i64, file_attributes: u32) -> Vec<u8> {
        let mut v = vec![0u8; 48];
        put_u64(&mut v, 8, mtime as u64); // modification_time
        put_u32(&mut v, 32, file_attributes);
        resident_attr(T_STD_INFO, &v)
    }

    fn file_name(parent_frn: u64, namespace: u8, name: &[u16]) -> Vec<u8> {
        let mut v = vec![0u8; 66 + name.len() * 2];
        let parent_reference = if parent_frn >> 48 == 0 {
            frn(parent_frn)
        } else {
            parent_frn
        };
        put_u64(&mut v, 0, parent_reference);
        v[64] = name.len() as u8; // name_length (code units)
        v[65] = namespace;
        for (i, u) in name.iter().enumerate() {
            put_u16(&mut v, 66 + i * 2, *u);
        }
        resident_attr(T_FILE_NAME, &v)
    }

    /// Spec for one synthetic `FILE` record; `None` attributes are simply
    /// omitted, so every parser branch (named/extension/deferred/skipped) is
    /// reachable by leaving fields out.
    #[derive(Default)]
    struct Rec {
        sequence: u16,
        flags_extra: u16,
        base_reference: u64,
        /// Set false to deliberately fail the fixup (torn-record path).
        good_fixup: bool,
        /// Set false to clear the `IN_USE` flag (free record).
        in_use: bool,
        attrs: Vec<Vec<u8>>,
    }

    impl Rec {
        fn new() -> Self {
            Self {
                sequence: 1,
                good_fixup: true,
                in_use: true,
                ..Default::default()
            }
        }
        fn dir(mut self) -> Self {
            self.flags_extra |= F_DIR;
            self
        }
        fn attr(mut self, a: Vec<u8>) -> Self {
            self.attrs.push(a);
            self
        }
        fn base(mut self, base_reference: u64) -> Self {
            self.base_reference = base_reference;
            self
        }

        /// Serialize to a `REC`-byte record, including the update-sequence
        /// array and the per-sector fixup sentinels that `apply_fixup` checks.
        fn build(&self) -> Vec<u8> {
            const USA_OFFSET: usize = 48;
            const USA_LENGTH: u16 = (SECTORS + 1) as u16; // 1 USN + one per sector
            const ATTRS_OFFSET: usize = 56;
            let mut r = vec![0u8; REC];
            r[0..4].copy_from_slice(b"FILE");
            put_u16(&mut r, 4, USA_OFFSET as u16);
            put_u16(&mut r, 6, USA_LENGTH);
            put_u16(&mut r, 16, self.sequence);
            put_u16(&mut r, 18, 1); // link_count
            put_u16(&mut r, 20, ATTRS_OFFSET as u16);
            let mut flags = self.flags_extra;
            if self.in_use {
                flags |= F_IN_USE;
            }
            put_u16(&mut r, 22, flags);
            put_u32(&mut r, 28, REC as u32); // allocated_size
            put_u64(&mut r, 32, self.base_reference);

            let mut off = ATTRS_OFFSET;
            let mut used_ids: Vec<u16> = self
                .attrs
                .iter()
                .map(|attribute| u16::from_le_bytes([attribute[14], attribute[15]]))
                .filter(|id| *id != 0)
                .collect();
            let mut next_implicit_id = 0u16;
            for a in &self.attrs {
                r[off..off + a.len()].copy_from_slice(a);
                let declared_id = u16::from_le_bytes([a[14], a[15]]);
                if declared_id == 0 {
                    while used_ids.contains(&next_implicit_id) {
                        next_implicit_id = next_implicit_id
                            .checked_add(1)
                            .expect("test fixture cannot exhaust u16 attribute ids");
                    }
                    put_u16(&mut r, off + 14, next_implicit_id);
                    used_ids.push(next_implicit_id);
                }
                off += a.len();
            }
            put_u32(&mut r, off, T_END); // terminating attribute marker
            put_u32(&mut r, 24, (off + 8) as u32); // used_size

            // Update-sequence array: USN then one (zero) fixup per sector. The
            // real sector-tail bytes are padding zeros, so the stored fixups
            // are zero; the sentinel is written into each sector tail.
            put_u16(&mut r, USA_OFFSET, USN_SENTINEL);
            for s in 1..=SECTORS {
                let tail = s * 512 - 2;
                let sentinel = if self.good_fixup {
                    USN_SENTINEL
                } else {
                    // A tail that does not match the USN ⇒ torn record.
                    USN_SENTINEL ^ 0xFFFF
                };
                put_u16(&mut r, tail, sentinel);
            }
            r
        }
    }

    /// First record number of a subrange placed so the leading records are
    /// past the reserved metafiles (< `FIRST_NORMAL_RECORD`).
    fn logical_at(record: u64) -> u64 {
        record * REC as u64
    }

    fn name_of<'b>(batch: &'b ParsedBatch, m: &ParsedMeta) -> &'b [u8] {
        &batch.name_pool[m.name_off as usize..(m.name_off + m.name_len) as usize]
    }

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Build a one-record subrange starting at `record` and parse it.
    fn parse_one(record: u64, rec: &Rec) -> ParsedBatch {
        let mut bytes = rec.build();
        parse_subrange(&mut bytes, logical_at(record), REC, 512)
    }

    #[test]
    fn deferred_record_arena_cap_is_shared_and_base_spills_keep_only_number() {
        let mut batch = ParsedBatch {
            rec_pool: vec![0u8; REC * 2],
            ..Default::default()
        };
        batch.extensions.push((42, 0..REC));
        batch.deferred.push((43, REC..REC * 2));

        let mut builder = VolumeIndexBuilder::new_synthetic("C:", 5);
        let mut stats = ScanStats::default();
        let mut deferred = Vec::new();
        let mut extensions = FxHashMap::default();
        let mut arena = RecordArena::new(REC);
        append_batches_bounded(
            &mut builder,
            &mut stats,
            &mut deferred,
            &mut extensions,
            &mut arena,
            vec![batch],
            REC,
        );

        assert_eq!(arena.data.len(), REC, "the byte ceiling is hard");
        assert_eq!(extensions.get(&42), Some(&0));
        assert_eq!(deferred, vec![(43, None)]);
        assert_eq!(stats.deferred_record_cache_spills, 1);
    }

    // ── A plain file: name, counts, parent, attributes all land ──────────────

    #[test]
    fn plain_file_record_parses_name_parent_and_size() {
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_WIN32, &utf16("report.txt")))
            .attr(data_nonresident(4096));
        let batch = parse_one(30, &rec);

        assert_eq!(batch.files, 1);
        assert_eq!(batch.dirs, 0);
        assert_eq!(batch.metas.len(), 1);
        let m = &batch.metas[0];
        assert_eq!(name_of(&batch, m), b"report.txt");
        assert_eq!(m.parent_frn, frn(5));
        assert_eq!(m.frn, frn(30)); // record 30, sequence 1 in the top 16 bits
        assert!(!m.is_dir);
        assert_eq!(m.attrs.size, 4096);
        assert!(!m.attrs.is_hidden && !m.attrs.is_system && !m.attrs.is_reparse);
    }

    #[test]
    fn directory_record_counts_as_dir() {
        let rec =
            Rec::new()
                .dir()
                .attr(std_info(0, 0))
                .attr(file_name(5, NS_WIN32, &utf16("docs")));
        let batch = parse_one(31, &rec);
        assert_eq!(batch.dirs, 1);
        assert_eq!(batch.files, 0);
        assert!(batch.metas[0].is_dir);
    }

    #[test]
    fn resident_data_size_is_the_value_length() {
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_WIN32, &utf16("small.bin")))
            .attr(resident_attr(T_DATA, &[0u8; 100]));
        let batch = parse_one(32, &rec);
        assert_eq!(batch.metas[0].attrs.size, 100);
    }

    #[test]
    fn data_sizes_are_decoded_from_misaligned_attribute_bytes() {
        let resident_bytes = resident_attr(T_DATA, &[0u8; 257]);
        let mut misaligned_resident = vec![0xA5];
        misaligned_resident.extend_from_slice(&resident_bytes);
        let resident = NtfsAttribute::parse(&misaligned_resident[1..])
            .expect("resident attribute should be structurally valid");
        assert_eq!(unnamed_data_size(&resident), Some(257));

        let expected = 0x0123_4567_89AB_CDEF;
        let nonresident_bytes = data_nonresident(expected);
        let mut misaligned_nonresident = vec![0x5A];
        misaligned_nonresident.extend_from_slice(&nonresident_bytes);
        let nonresident = NtfsAttribute::parse(&misaligned_nonresident[1..])
            .expect("non-resident attribute should be structurally valid");
        assert_eq!(unnamed_data_size(&nonresident), Some(expected));
    }

    #[test]
    fn named_data_before_unnamed_data_does_not_change_file_size() {
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_WIN32, &utf16("streams.bin")))
            .attr(named_resident_attr(
                T_DATA,
                &utf16("Zone.Identifier"),
                &[0u8; 17],
            ))
            .attr(data_nonresident(4096));
        let batch = parse_one(32, &rec);
        assert_eq!(batch.metas[0].attrs.size, 4096);
    }

    #[test]
    fn named_data_after_unnamed_data_does_not_overwrite_file_size() {
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_WIN32, &utf16("streams.bin")))
            .attr(data_nonresident(4096))
            .attr(named_resident_attr(
                T_DATA,
                &utf16("Zone.Identifier"),
                &[0u8; 17],
            ));
        let batch = parse_one(32, &rec);
        assert_eq!(batch.metas[0].attrs.size, 4096);
    }

    // ── $STANDARD_INFORMATION attribute bits flow into RecordAttrs ───────────

    #[test]
    fn standard_information_flags_and_mtime_are_extracted() {
        let rec = Rec::new()
            .attr(std_info(
                0x01DC_BEEF,
                A_HIDDEN | A_SYSTEM | A_REPARSE | A_ARCHIVE,
            ))
            .attr(file_name(5, NS_WIN32, &utf16("hidden.sys")));
        let batch = parse_one(33, &rec);
        let a = batch.metas[0].attrs;
        assert!(a.is_hidden);
        assert!(a.is_system);
        assert!(a.is_reparse);
        assert_eq!(a.mtime, 0x01DC_BEEF);
    }

    // ── Name selection: namespace preference & WTF-8 edge cases ──────────────

    #[test]
    fn dos_only_name_without_attribute_list_is_skipped_no_name() {
        // A DOS (8.3) short name alone is never the display name; with no
        // $ATTRIBUTE_LIST to defer to, the record is counted skipped.
        let rec = Rec::new().attr(std_info(0, A_ARCHIVE)).attr(file_name(
            5,
            NS_DOS,
            &utf16("LONGFI~1.TXT"),
        ));
        let batch = parse_one(34, &rec);
        assert_eq!(batch.metas.len(), 0);
        assert_eq!(batch.skipped_no_name, 1);
        assert_eq!(batch.deferred.len(), 0);
    }

    #[test]
    fn win32_name_is_preferred_over_a_dos_name() {
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_DOS, &utf16("LONGFI~1.TXT")))
            .attr(file_name(5, NS_WIN32, &utf16("long file.txt")));
        let batch = parse_one(35, &rec);
        assert_eq!(batch.metas.len(), 1);
        assert_eq!(name_of(&batch, &batch.metas[0]), b"long file.txt");
    }

    #[test]
    fn every_searchable_hard_link_in_the_base_record_is_emitted() {
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_WIN32, &utf16("first.txt")))
            .attr(file_name(9, NS_WIN32, &utf16("second.txt")));
        let batch = parse_one(35, &rec);
        assert_eq!(batch.files, 2);
        assert_eq!(batch.metas.len(), 2);
        assert_eq!(name_of(&batch, &batch.metas[0]), b"first.txt");
        assert_eq!(batch.metas[0].parent_frn, frn(5));
        assert_eq!(name_of(&batch, &batch.metas[1]), b"second.txt");
        assert_eq!(batch.metas[1].parent_frn, frn(9));
        assert_eq!(batch.metas[0].frn, batch.metas[1].frn);
    }

    #[test]
    fn posix_name_is_accepted_as_a_fallback() {
        let rec = Rec::new().attr(std_info(0, A_ARCHIVE)).attr(file_name(
            5,
            NS_POSIX,
            &utf16("posix.name"),
        ));
        let batch = parse_one(36, &rec);
        assert_eq!(name_of(&batch, &batch.metas[0]), b"posix.name");
    }

    #[test]
    fn win32_and_dos_combined_namespace_is_kept() {
        let rec = Rec::new().attr(std_info(0, A_ARCHIVE)).attr(file_name(
            5,
            NS_WIN32_DOS,
            &utf16("both.txt"),
        ));
        let batch = parse_one(37, &rec);
        assert_eq!(name_of(&batch, &batch.metas[0]), b"both.txt");
    }

    #[test]
    fn lone_surrogate_name_round_trips_through_wtf8() {
        // A UTF-16 name carrying an unpaired surrogate (0xD800) must survive as
        // WTF-8 rather than being lost or replaced — the codec's reason to
        // exist. The lower pool gets the folded copy at the same byte length.
        let name = vec![b'a' as u16, 0xD800, b'z' as u16];
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(file_name(5, NS_WIN32, &name));
        let batch = parse_one(38, &rec);
        let m = &batch.metas[0];
        let bytes = name_of(&batch, m);
        let mut round = Vec::new();
        crate::wtf8::wtf8_to_utf16(bytes, &mut round);
        assert_eq!(round, name, "WTF-8 round-trips through the name pool");
        // The lower (folded) pool is populated alongside the original.
        assert_eq!(
            batch.lower_pool[m.name_off as usize..(m.name_off + m.name_len) as usize].len(),
            bytes.len()
        );
    }

    // ── Record-classification branches ───────────────────────────────────────

    #[test]
    fn metafile_records_below_first_normal_record_are_skipped() {
        // Record numbers < FIRST_NORMAL_RECORD (24) are NTFS metafiles; the
        // builder seeds the root itself, so the parser must skip them.
        let rec =
            Rec::new()
                .attr(std_info(0, A_ARCHIVE))
                .attr(file_name(5, NS_WIN32, &utf16("$Secure")));
        let batch = parse_one(9, &rec); // record 9 ($Secure) < 24
        assert_eq!(batch.metas.len(), 0);
        assert_eq!(batch.files, 0);
        assert_eq!(batch.skipped_no_name, 0, "skipped before name handling");
    }

    #[test]
    fn extend_is_indexed_so_its_children_have_a_parent_to_resolve() {
        // `\$Extend` (record 11) is the one metafile that must NOT be skipped.
        // Its children — $Quota (24), $ObjId (25), $Reparse (26), $UsnJrnl,
        // $RmMetadata — live at or above FIRST_NORMAL_RECORD and are indexed,
        // and $RmMetadata is itself a directory, so the subtree extends further
        // still. Skipping record 11 leaves every one of those rows naming a
        // parent that resolves to nothing, and the strict builder turns a
        // single unresolved exact parent into a whole-volume failure. A real C:
        // failed exactly this way: "entry 1 (Frn(281474976710680)) has
        // unresolved exact parent Frn(3096224743817227)" — $Extend\$Quota
        // pointing at $Extend.
        let rec =
            Rec::new()
                .attr(std_info(0, A_ARCHIVE))
                .attr(file_name(5, NS_WIN32, &utf16("$Extend")));
        let batch = parse_one(11, &rec);
        assert_eq!(batch.metas.len(), 1, "$Extend must be indexed");
        assert_eq!(name_of(&batch, &batch.metas[0]), b"$Extend");
        assert_eq!(
            batch.metas[0].parent_frn,
            frn(5),
            "$Extend hangs off the root the builder seeds"
        );
    }

    #[test]
    fn unused_record_is_skipped_silently() {
        let mut rec = Rec::new().attr(std_info(0, A_ARCHIVE)).attr(file_name(
            5,
            NS_WIN32,
            &utf16("deleted.txt"),
        ));
        rec.in_use = false;
        let batch = parse_one(40, &rec);
        assert_eq!(batch.metas.len(), 0);
        assert_eq!(batch.files, 0);
        assert_eq!(batch.corrupt_records, 0);
        assert_eq!(batch.skipped_no_name, 0);
    }

    #[test]
    fn torn_record_fails_fixup_and_counts_corrupt() {
        let mut rec = Rec::new().attr(std_info(0, A_ARCHIVE)).attr(file_name(
            5,
            NS_WIN32,
            &utf16("torn.txt"),
        ));
        rec.good_fixup = false;
        let batch = parse_one(41, &rec);
        assert_eq!(batch.corrupt_records, 1);
        assert_eq!(batch.metas.len(), 0);
    }

    #[test]
    fn only_zero_unallocated_slots_are_silently_skipped() {
        let mut zero = vec![0u8; REC];
        let zero_batch = parse_subrange(&mut zero, logical_at(41), REC, 512);
        assert_eq!(zero_batch.corrupt_records, 0);
        assert!(zero_batch.metas.is_empty());

        let mut invalid = vec![0u8; REC];
        invalid[..4].copy_from_slice(b"BAAD");
        let invalid_batch = parse_subrange(&mut invalid, logical_at(41), REC, 512);
        assert_eq!(invalid_batch.corrupt_records, 1);
        assert!(invalid_batch.metas.is_empty());
    }

    #[test]
    fn extension_record_without_a_name_is_counted_only() {
        // base_reference's low 48 bits non-zero ⇒ this is a fragment of another
        // file. Without a $FILE_NAME it is just counted, never indexed.
        let rec = Rec::new().base(frn(30)).attr(data_nonresident(8192));
        let batch = parse_one(42, &rec);
        assert_eq!(batch.extension_records, 1);
        assert_eq!(batch.extensions.len(), 0);
        assert_eq!(batch.metas.len(), 0);
    }

    #[test]
    fn extension_record_with_a_name_is_stashed_for_the_deferred_pass() {
        let rec = Rec::new()
            .base(frn(30))
            .attr(file_name(5, NS_WIN32, &utf16("fragment.txt")));
        let batch = parse_one(43, &rec);
        assert_eq!(batch.extension_records, 1);
        assert_eq!(batch.extensions.len(), 1);
        assert_eq!(batch.extensions[0].0, 43); // keyed by record number
        assert_eq!(batch.metas.len(), 0);
    }

    #[test]
    fn base_record_needing_attribute_list_is_deferred() {
        // No usable $FILE_NAME in the base record but an $ATTRIBUTE_LIST is
        // present ⇒ the name lives in an extension record; defer resolution.
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(resident_attr(T_ATTR_LIST, &[0u8; 24]));
        let batch = parse_one(44, &rec);
        assert_eq!(batch.deferred.len(), 1);
        assert_eq!(batch.deferred[0].0, frn(44));
        assert_eq!(batch.skipped_no_name, 0);
        assert_eq!(batch.metas.len(), 0);
    }

    #[test]
    fn base_record_with_a_direct_name_and_attribute_list_is_still_deferred() {
        // Attributes are stored in ascending type-id order, so $ATTRIBUTE_LIST
        // (0x20) precedes $FILE_NAME (0x30) in the record.
        let rec = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(resident_attr(T_ATTR_LIST, &[0u8; 24]))
            .attr(file_name(5, NS_WIN32, &utf16("base-link.txt")));
        let batch = parse_one(44, &rec);
        assert_eq!(batch.deferred.len(), 1);
        assert!(batch.metas.is_empty());
        assert_eq!(batch.files, 0);
    }

    #[test]
    fn deferred_attribute_list_resolves_a_valid_extension_name() {
        let base_reference = frn(70);
        let extension_reference = frn(71);
        let list = attribute_list_entry(T_FILE_NAME, extension_reference, 2);
        let base = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(attribute_id(resident_attr(T_ATTR_LIST, &list), 3));
        let extension = Rec::new().base(base_reference).attr(attribute_id(
            file_name(9, NS_WIN32, &utf16("extension-link.txt")),
            2,
        ));

        let mut builder = VolumeIndexBuilder::new_synthetic("C:", 5);
        let mut stats = ScanStats::default();
        let mut deferred = Vec::new();
        let mut extensions = FxHashMap::default();
        let mut arena = RecordArena::new(REC);
        append_batches(
            &mut builder,
            &mut stats,
            &mut deferred,
            &mut extensions,
            &mut arena,
            vec![parse_one(70, &base), parse_one(71, &extension)],
        );

        let runmap = super::super::volume_io::RunMap { runs: Vec::new() };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let metadata = crate::usn::MetadataSource::constant(0, 0);
        let resolved = super::super::deferred::resolve_deferred(
            super::super::deferred::DeferredContext {
                volume_path: "not-opened-for-resident-fixture",
                runmap: &runmap,
                record_size: REC,
                sector_size: 512,
                cluster_size: 4096,
                volume_size: 1 << 20,
                extensions: &extensions,
                arena: &arena,
                metadata: &metadata,
                stop: &stop,
            },
            &deferred,
        )
        .unwrap();
        let names: Vec<Vec<u8>> = resolved
            .iter()
            .flat_map(|batch| batch.metas.iter().map(|meta| name_of(batch, meta).to_vec()))
            .collect();
        assert_eq!(names, [b"extension-link.txt".to_vec()]);
        assert_eq!(resolved[0].files, 1);
    }

    #[test]
    fn deferred_failure_uses_complete_live_links_or_fails_the_scan() {
        use std::collections::HashMap;

        let reference = frn(72);
        let missing_extension = frn(73);
        let mut list = attribute_list_entry(T_FILE_NAME, reference, 1);
        list.extend_from_slice(&attribute_list_entry(T_FILE_NAME, missing_extension, 2));
        // Ascending attribute-type order: 0x10, 0x20, then 0x30.
        let base = Rec::new()
            .attr(std_info(0, A_ARCHIVE))
            .attr(attribute_id(resident_attr(T_ATTR_LIST, &list), 3))
            .attr(attribute_id(
                file_name(5, NS_WIN32, &utf16("stale-base.txt")),
                1,
            ));

        let mut builder = VolumeIndexBuilder::new_synthetic("C:", 5);
        let mut stats = ScanStats::default();
        let mut deferred = Vec::new();
        let mut extensions = FxHashMap::default();
        let mut arena = RecordArena::new(REC);
        append_batches(
            &mut builder,
            &mut stats,
            &mut deferred,
            &mut extensions,
            &mut arena,
            vec![parse_one(72, &base)],
        );
        let runmap = super::super::volume_io::RunMap { runs: Vec::new() };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let live = crate::usn::MetadataSource::map_with_links(
            HashMap::from([(reference, (42, 9))]),
            HashMap::from([(
                reference,
                vec![
                    crate::usn::LinkInfo {
                        parent_frn: frn(7),
                        name: utf16("live-first.txt"),
                    },
                    crate::usn::LinkInfo {
                        parent_frn: frn(8),
                        name: utf16("live-second.txt"),
                    },
                ],
            )]),
        );
        macro_rules! context {
            ($metadata:expr) => {
                super::super::deferred::DeferredContext {
                    volume_path: "missing-extension-fixture",
                    runmap: &runmap,
                    record_size: REC,
                    sector_size: 512,
                    cluster_size: 4096,
                    volume_size: 1 << 20,
                    extensions: &extensions,
                    arena: &arena,
                    metadata: $metadata,
                    stop: &stop,
                }
            };
        }

        let resolved =
            super::super::deferred::resolve_deferred(context!(&live), &deferred).unwrap();
        let names: Vec<Vec<u8>> = resolved[0]
            .metas
            .iter()
            .map(|meta| name_of(&resolved[0], meta).to_vec())
            .collect();
        assert_eq!(
            names,
            [b"live-first.txt".to_vec(), b"live-second.txt".to_vec()]
        );

        // An object the live source can name but cannot size is the
        // `\$Extend\$ObjId` shape: past `FIRST_NORMAL_RECORD`, carrying an
        // $ATTRIBUTE_LIST, and refused by `OpenFileById` on every real volume.
        // The row must still be published — with the size its base record
        // proves — because one unsizable object must not cost the whole index.
        let unsizable = crate::usn::MetadataSource::map_with_links(
            HashMap::new(),
            HashMap::from([(
                reference,
                vec![crate::usn::LinkInfo {
                    parent_frn: frn(7),
                    name: utf16("live-first.txt"),
                }],
            )]),
        );
        let degraded =
            super::super::deferred::resolve_deferred(context!(&unsizable), &deferred).unwrap();
        assert_eq!(degraded[0].deferred_stat_failures, 1);
        let degraded_names: Vec<Vec<u8>> = degraded[0]
            .metas
            .iter()
            .map(|meta| name_of(&degraded[0], meta).to_vec())
            .collect();
        assert_eq!(degraded_names, [b"live-first.txt".to_vec()]);

        // A name, by contrast, is not optional: with no authoritative link set
        // the row cannot be published at all, so this one stays fatal.
        let unavailable = crate::usn::MetadataSource::none();
        assert!(matches!(
            super::super::deferred::resolve_deferred(context!(&unavailable), &deferred),
            Err(super::super::deferred::DeferredError::Incomplete(found))
                if found.reference == reference
                    && found.cause == crate::mft::IncompleteCause::LinkSetUnavailable
        ));

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            super::super::deferred::resolve_deferred(context!(&live), &deferred),
            Err(super::super::deferred::DeferredError::Cancelled)
        ));
    }

    // ── Multi-record subrange & the parallel/sequential determinism oracle ───

    #[test]
    fn many_records_in_one_subrange_keep_record_order() {
        let mut bytes = Vec::new();
        let names = ["a.rs", "b.rs", "c.rs", "d.rs"];
        for (i, n) in names.iter().enumerate() {
            let rec =
                Rec::new()
                    .attr(std_info(0, A_ARCHIVE))
                    .attr(file_name(5, NS_WIN32, &utf16(n)));
            // Pad each record to REC and append (parse walks by record_size).
            let mut r = rec.build();
            bytes.append(&mut r);
            let _ = i;
        }
        let batch = parse_subrange(&mut bytes, logical_at(50), REC, 512);
        let got: Vec<&[u8]> = batch.metas.iter().map(|m| name_of(&batch, m)).collect();
        let want: Vec<&[u8]> = names.iter().map(|n| n.as_bytes()).collect();
        assert_eq!(got, want);
        assert_eq!(batch.files, 4);
    }

    #[test]
    fn parse_chunk_split_matches_a_single_sequential_subrange() {
        // `parse_chunk` fans a chunk across rayon workers in 1 MiB sub-ranges.
        // With > 1 MiB of records the split is real (≥ 2 sub-ranges); the
        // concatenated result must equal one sequential parse of the whole
        // chunk — the determinism the doc comment promises ("Mirrors the
        // sequential loop exactly").
        const COUNT: u64 = 1100; // 1100 KiB > 1 MiB ⇒ forces a multi-way split
        let mut chunk = Vec::with_capacity(COUNT as usize * REC);
        for i in 0..COUNT {
            let nm = format!("file_{i}.dat");
            let rec = Rec::new()
                .attr(std_info(i as i64, A_ARCHIVE))
                .attr(file_name(5, NS_WIN32, &utf16(&nm)));
            chunk.extend_from_slice(&rec.build());
        }
        let first = logical_at(24);

        let mut parallel_input = chunk.clone();
        let batches = parse_chunk(&mut parallel_input, first, REC, 512);
        assert!(batches.len() >= 2, "the 1 MiB split must actually fan out");
        let parallel: Vec<(u64, Vec<u8>)> = batches
            .iter()
            .flat_map(|b| b.metas.iter().map(move |m| (m.frn, name_of(b, m).to_vec())))
            .collect();

        let mut seq_input = chunk;
        let seq_batch = parse_subrange(&mut seq_input, first, REC, 512);
        let sequential: Vec<(u64, Vec<u8>)> = seq_batch
            .metas
            .iter()
            .map(|m| (m.frn, name_of(&seq_batch, m).to_vec()))
            .collect();

        assert_eq!(parallel.len(), COUNT as usize);
        assert_eq!(
            parallel, sequential,
            "chunked parse must equal a sequential parse, in order"
        );
    }

    // ── append_batches: name pools and counters fold into the index/stats ────

    #[test]
    fn append_batches_builds_an_index_and_folds_stats() {
        let mut bytes = Vec::new();
        for n in ["one.txt", "two.txt"] {
            let rec =
                Rec::new()
                    .attr(std_info(0, A_ARCHIVE))
                    .attr(file_name(5, NS_WIN32, &utf16(n)));
            bytes.extend_from_slice(&rec.build());
        }
        let batch = parse_subrange(&mut bytes, logical_at(60), REC, 512);

        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let mut stats = ScanStats::default();
        let mut deferred = Vec::new();
        let mut extensions = FxHashMap::default();
        let mut arena = RecordArena::new(REC);
        append_batches(
            &mut b,
            &mut stats,
            &mut deferred,
            &mut extensions,
            &mut arena,
            vec![batch],
        );
        assert_eq!(stats.files, 2);
        let idx = b.finish();
        let names: Vec<String> = (0..idx.len() as u32)
            .filter(|&id| idx.is_live(id))
            .map(|id| String::from_utf8_lossy(idx.name(id)).into_owned())
            .collect();
        assert!(names.contains(&"one.txt".to_string()));
        assert!(names.contains(&"two.txt".to_string()));
    }
}
