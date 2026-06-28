//! Parallel chunk parsing (ADR-0011): record sub-ranges of one chunk fan
//! out across rayon workers, each producing a [`ParsedBatch`]; the builder
//! appends the batches in chunk order, so `EntryId` assignment is
//! deterministic.

use ntfs_reader::api::{FIRST_NORMAL_RECORD, NtfsAttributeType, NtfsFileName};
use ntfs_reader::file::NtfsFile;
use rustc_hash::FxHashMap;

use crate::index::{EncodedEntry, Frn, VolumeIndexBuilder};
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
        let is_dir = f.is_directory();
        if is_dir {
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
            parent_frn: name.header.parent_directory_reference,
            frn: f.reference_number(),
            name_off,
            name_len: self.name_pool.len() as u32 - name_off,
            is_dir,
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

#[cfg(test)]
mod tests {
    //! Byte-fixture replay of the $MFT parse path — the analogue of
    //! `tests/usn_replay.rs` for the scan side. Synthetic NTFS `FILE` records
    //! are built byte-for-byte from the documented on-disk layout (the
    //! `ntfs_reader::api` `#[repr(C, packed)]` structs) and run through the
    //! real `parse_subrange` / `parse_chunk` / `append_batches` — no OS, no
    //! elevation, no seam. This covers `scan/parse.rs`, which the MFT-scan
    //! privilege barrier otherwise leaves to elevated `FMF_ADMIN_TESTS` only.
    //!
    //! Layout references (all little-endian, from ntfs-reader 0.4.5):
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

    // NTFS attribute type ids (ntfs_reader::api::NtfsAttributeType).
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

    /// A non-resident $DATA attribute carrying only the geometry the parser
    /// reads (`data_size` @48); no real data runs are needed.
    fn data_nonresident(data_size: u64) -> Vec<u8> {
        let length = 64usize; // size_of::<NtfsNonResidentAttributeHeader>()
        let mut a = vec![0u8; length];
        put_u32(&mut a, 0, T_DATA);
        put_u32(&mut a, 4, length as u32);
        a[8] = 1; // is_non_resident
        put_u16(&mut a, 34, 64); // data_runs_offset (past the header; unused)
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
        put_u64(&mut v, 0, parent_frn); // parent_directory_reference
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
            for a in &self.attrs {
                r[off..off + a.len()].copy_from_slice(a);
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
        parse_subrange(&mut bytes, logical_at(record), REC)
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
        assert_eq!(m.parent_frn, 5);
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
        let batch = parse_one(11, &rec); // record 11 < 24
        assert_eq!(batch.metas.len(), 0);
        assert_eq!(batch.files, 0);
        assert_eq!(batch.skipped_no_name, 0, "skipped before name handling");
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
        assert_eq!(batch.deferred[0].0, 44);
        assert_eq!(batch.skipped_no_name, 0);
        assert_eq!(batch.metas.len(), 0);
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
        let batch = parse_subrange(&mut bytes, logical_at(50), REC);
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
        let batches = parse_chunk(&mut parallel_input, first, REC);
        assert!(batches.len() >= 2, "the 1 MiB split must actually fan out");
        let parallel: Vec<(u64, Vec<u8>)> = batches
            .iter()
            .flat_map(|b| b.metas.iter().map(move |m| (m.frn, name_of(b, m).to_vec())))
            .collect();

        let mut seq_input = chunk;
        let seq_batch = parse_subrange(&mut seq_input, first, REC);
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
        let batch = parse_subrange(&mut bytes, logical_at(60), REC);

        let mut b = VolumeIndexBuilder::new("C:", 5);
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
