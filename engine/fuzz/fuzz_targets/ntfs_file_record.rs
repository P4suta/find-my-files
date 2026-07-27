#![no_main]
//! Fuzz one whole $MFT `FILE` record through the pipeline the scanner really
//! runs: update-sequence fixup, whole-chain completeness validation, then
//! attribute-by-attribute decoding. Every byte comes off the raw volume, so a
//! crafted disk owns all of it and `fmf-service` parses it as `LocalSystem`
//! (ADR-0047).
//!
//! Input shaping — the point of this harness. Random bytes stop at the `FILE`
//! signature; even past it the update-sequence geometry, the
//! `allocated_size == buffer length` equality and the `attributes_offset <
//! used_size` ordering are conjunctions a mutator will not stumble into, and
//! the corpus starts empty on every CI run so depth has to come from the
//! harness rather than from corpus evolution. Three constructions, cheapest
//! first:
//!
//! 1. the raw bytes, so the fail-closed paths stay under coverage;
//! 2. `base_record` — the record *header* stamped into a shape the validators
//!    accept, with the entire attribute region left as fuzz bytes. This is what
//!    reaches the chain walk in `attributes_complete` and `NtfsFile::attributes`
//!    with attacker-chosen type codes, lengths and residency flags;
//! 3. `frame_chain` — additionally lays a well-formed three-attribute skeleton
//!    (`$STANDARD_INFORMATION`, `$FILE_NAME`, non-resident `$DATA`) over that
//!    region, stamping only the structural fields that gate entry. The values
//!    stay exactly as the fuzzer wrote them, so `as_name` reaches its NUL and
//!    path-separator scan, its parent-reference and namespace checks, and
//!    `nonresident_header` reaches its VCN and data-size arithmetic — none of
//!    which construction 2 hits with any useful frequency.
//!
//! Constructions 2 and 3 are each also run through `apply_fixup` with the
//! sector tails carrying the update-sequence number, which is the only way the
//! fixup's success path (and the reparse of a genuinely fixed-up buffer) is
//! exercised; the sentinel bytes swapped in are fuzz bytes from the record's
//! own update-sequence array.

use fmf_core::ondisk::fixup::apply_fixup;
use fmf_core::ondisk::ntfs::{NtfsAttributeType, NtfsFile};
use fmf_core::ondisk::record::attributes_complete;
use libfuzzer_sys::fuzz_target;

/// `(record bytes, sector size)` pairs a real boot sector can declare.
const GEOMETRIES: [(usize, usize); 5] = [
    (512, 512),
    (1024, 512),
    (2048, 512),
    (4096, 512),
    (4096, 4096),
];
/// Where the update-sequence array is placed: past the 42-byte header, even.
const USA_OFFSET: usize = 48;
const RESIDENT_HEADER_BYTES: usize = 24;
const NONRESIDENT_HEADER_BYTES: usize = 64;
const FILE_NAME_HEADER_BYTES: usize = 66;
const ATTRIBUTE_END: u32 = 0xFFFF_FFFF;

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn le_u64(data: &[u8], offset: usize) -> u64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(out)
}

/// Byte offset of the first attribute for a record of this geometry.
const fn attributes_offset_for(record_bytes: usize, sector_size: usize) -> usize {
    let update_sequence_length = record_bytes / sector_size + 1;
    (USA_OFFSET + update_sequence_length * 2).next_multiple_of(8)
}

/// A record whose header the validators accept, with `body` as its contents.
///
/// Only the fields that are pure conjunctions — signature, update-sequence
/// geometry, `allocated_size`, the sequence/base-reference pairing rules and
/// the `used_size` range — are repaired. The flags word, the base reference's
/// low bits, `used_size`'s exact value and the whole attribute region stay
/// under fuzzer control.
fn base_record(record_bytes: usize, sector_size: usize, body: &[u8]) -> Vec<u8> {
    let mut record = vec![0u8; record_bytes];
    let taken = body.len().min(record_bytes);
    record[..taken].copy_from_slice(&body[..taken]);

    let update_sequence_length = record_bytes / sector_size + 1;
    let attributes_offset = attributes_offset_for(record_bytes, sector_size);

    record[..4].copy_from_slice(b"FILE");
    put_u16(&mut record, 4, USA_OFFSET as u16);
    put_u16(&mut record, 6, update_sequence_length as u16);
    if le_u16(&record, 16) == 0 {
        // An in-use record must carry a non-zero sequence value; repairing it
        // unconditionally keeps both the in-use and the deleted shape reachable
        // through the (fuzz-controlled) flags word instead of this field.
        put_u16(&mut record, 16, 1);
    }
    put_u16(&mut record, 20, attributes_offset as u16);
    // Keep the fuzzer's bits but land inside (attributes_offset, record_bytes].
    let span = record_bytes - attributes_offset;
    let used = attributes_offset + 1 + (le_u32(&record, 24) as usize % span);
    put_u32(&mut record, 24, used as u32);
    put_u32(&mut record, 28, record_bytes as u32);
    let base_reference = le_u64(&record, 32);
    if base_reference != 0 && base_reference >> 48 == 0 {
        put_u64(&mut record, 32, base_reference | (1 << 48));
    }
    record
}

/// Stamp a resident attribute header over bytes the fuzzer already wrote; the
/// value itself is left untouched.
fn resident_header(
    record: &mut [u8],
    offset: usize,
    type_id: u32,
    length: usize,
    value_length: usize,
    id: u16,
) {
    put_u32(record, offset, type_id);
    put_u32(record, offset + 4, length as u32);
    record[offset + 8] = 0; // resident
    record[offset + 9] = 0; // unnamed
    put_u16(record, offset + 10, 0);
    put_u16(record, offset + 12, 0); // unflagged
    put_u16(record, offset + 14, id);
    put_u32(record, offset + 16, value_length as u32);
    put_u16(record, offset + 20, RESIDENT_HEADER_BYTES as u16);
}

/// Stamp a non-resident attribute header; the VCN pair, the sizes and the
/// mapping pairs after it all stay as the fuzzer wrote them.
fn nonresident_header(record: &mut [u8], offset: usize, type_id: u32, length: usize, id: u16) {
    put_u32(record, offset, type_id);
    put_u32(record, offset + 4, length as u32);
    record[offset + 8] = 1; // non-resident
    record[offset + 9] = 0; // unnamed
    put_u16(record, offset + 10, 0);
    put_u16(record, offset + 12, 0); // unflagged
    put_u16(record, offset + 14, id);
    put_u16(record, offset + 32, NONRESIDENT_HEADER_BYTES as u16);
}

/// Lay a complete, ordered attribute chain over the record's attribute region.
///
/// The longest chain this can produce is 376 bytes plus its eight-byte
/// terminator margin, and the largest attribute offset any geometry yields is
/// 72, so the chain fits even the smallest (512-byte) record.
fn frame_chain(record: &mut [u8], attributes_offset: usize, name_units: u8, mapping_words: u8) {
    let mut offset = attributes_offset;

    // $STANDARD_INFORMATION: 72 bytes of fuzz-owned value, of which the decoder
    // reads the modification time and the attribute flags.
    resident_header(
        record,
        offset,
        NtfsAttributeType::StandardInformation as u32,
        96,
        72,
        1,
    );
    offset += 96;

    // $FILE_NAME only decodes when the resident value length equals
    // `66 + 2 * name_length` exactly, so stamp both sides of that equation.
    // Everything the decoder then inspects — parent reference, namespace, and
    // the UTF-16LE name it scans for NUL and path separators — stays fuzzed.
    let units = usize::from(name_units % 31) + 1;
    let value_length = FILE_NAME_HEADER_BYTES + units * 2;
    let length = (RESIDENT_HEADER_BYTES + value_length).next_multiple_of(8);
    resident_header(
        record,
        offset,
        NtfsAttributeType::FileName as u32,
        length,
        value_length,
        2,
    );
    let value = offset + RESIDENT_HEADER_BYTES;
    record[value + 64] = units as u8;
    // Narrow the namespace to 0..=7 so half the inputs are still the rejected
    // out-of-range case, and give an all-zero parent reference a sequence value
    // so a sparse input does not fail before the name is examined. Both of
    // those are single comparisons pinned by unit tests; the name scan behind
    // them — the part that does index arithmetic — stays entirely fuzzed.
    record[value + 65] &= 7;
    record[value + 7] |= 1;
    offset += length;

    // Non-resident $DATA: 8..=64 bytes of fuzz-owned mapping pairs.
    let length = NONRESIDENT_HEADER_BYTES + (usize::from(mapping_words % 8) + 1) * 8;
    nonresident_header(record, offset, NtfsAttributeType::Data as u32, length, 3);
    offset += length;

    put_u32(record, offset, ATTRIBUTE_END);
    put_u32(record, 24, (offset + 8) as u32); // used_size covers the terminator
}

/// Write the update-sequence number into every sector tail so `apply_fixup`
/// takes its success path and swaps in the array's (fuzz-chosen) replacements.
fn stamp_sector_sentinels(record: &mut [u8], sector_size: usize) {
    let update_sequence_number = [record[USA_OFFSET], record[USA_OFFSET + 1]];
    for sector in 1..=record.len() / sector_size {
        let tail = sector * sector_size - 2;
        record[tail..tail + 2].copy_from_slice(&update_sequence_number);
    }
}

/// Run everything a scanner does with one buffer it believes is a record.
fn decode(record: &[u8], sector_size: usize) {
    let _ = attributes_complete(record);
    let _ = NtfsFile::is_valid(record, sector_size);
    let Some(file) = NtfsFile::parse(7, record, sector_size) else {
        return;
    };
    let _ = file.reference_number();
    let _ = file.is_used();
    let _ = file.is_directory();
    for attribute_type in [
        NtfsAttributeType::StandardInformation,
        NtfsAttributeType::AttributeList,
        NtfsAttributeType::FileName,
        NtfsAttributeType::Data,
    ] {
        let _ = file.get_attribute(attribute_type);
    }
    file.attributes(|attribute| {
        let _ = attribute.len();
        let _ = attribute.data();
        let _ = attribute.get_resident();
        let _ = attribute.resident_value_length();
        let _ = attribute.nonresident_header();
        let _ = attribute.as_standard_info();
        if let Some(name) = attribute.as_name() {
            let _ = name.to_utf16();
        }
    });
}

fuzz_target!(|data: &[u8]| {
    // Fixed-offset control prefix: a byte's meaning must not shift when the
    // mutator changes the input's length, so the shape bytes are read from
    // known positions and the rest is the record body.
    let control = |index: usize| data.get(index).copied().unwrap_or(0);
    let (record_bytes, sector_size) = GEOMETRIES[usize::from(control(0)) % GEOMETRIES.len()];
    let body = data.get(3..).unwrap_or_default();

    // 1. Raw bytes: the fail-closed paths, including the fixup rejections.
    let _ = attributes_complete(data);
    let _ = NtfsFile::is_valid(data, sector_size);
    let mut raw = data.to_vec();
    let _ = apply_fixup(&mut raw, sector_size);

    // 2. Valid record header, attacker-owned attribute region.
    let mut record = base_record(record_bytes, sector_size, body);
    decode(&record, sector_size);
    let mut fixed = record.clone();
    stamp_sector_sentinels(&mut fixed, sector_size);
    if apply_fixup(&mut fixed, sector_size) {
        decode(&fixed, sector_size);
    }

    // 3. Same, plus a well-formed attribute skeleton so the per-attribute
    //    decoders run on attacker-owned values.
    let attributes_offset = attributes_offset_for(record_bytes, sector_size);
    frame_chain(&mut record, attributes_offset, control(1), control(2));
    decode(&record, sector_size);
    let mut fixed = record;
    stamp_sector_sentinels(&mut fixed, sector_size);
    if apply_fixup(&mut fixed, sector_size) {
        decode(&fixed, sector_size);
    }
});
