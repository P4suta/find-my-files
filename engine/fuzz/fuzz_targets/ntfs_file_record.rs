#![no_main]
//! Exercise raw and shaped records, checking fixup atomicity and attribute decoder consistency.

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
const ATTRIBUTE_ALIGNMENT: usize = 8;
const UPDATE_SEQUENCE_WORD_BYTES: usize = size_of::<u16>();
const USA_OFFSET_FIELD: usize = 4;
const USA_LENGTH_FIELD: usize = 6;
const SEQUENCE_OFFSET: usize = 16;
const ATTRIBUTES_OFFSET_FIELD: usize = 20;
const USED_BYTES_FIELD: usize = 24;
const ALLOCATED_BYTES_FIELD: usize = 28;
const BASE_REFERENCE_FIELD: usize = 32;
const SEQUENCE_SHIFT: u32 = u64::BITS - u16::BITS;
const RECORD_NUMBER_MASK: u64 = u64::MAX >> u16::BITS;
const LENGTH_FIELD: usize = 4;
const RESIDENCY_FIELD: usize = 8;
const NAME_UNITS_FIELD: usize = 9;
const NAME_OFFSET_FIELD: usize = 10;
const FLAGS_FIELD: usize = 12;
const ID_FIELD: usize = 14;
const RESIDENT_LENGTH_FIELD: usize = 16;
const RESIDENT_OFFSET_FIELD: usize = 20;
const MAPPING_OFFSET_FIELD: usize = 32;
const STANDARD_ATTRIBUTE_BYTES: usize = 96;
const STANDARD_VALUE_BYTES: usize = 72;
const FILE_NAME_ID: u16 = 2;
const DATA_ID: u16 = 3;
const MAX_SHAPED_NAME_UNITS: u8 = 31;
const NAME_LENGTH_FIELD: usize = 64;
const NAME_NAMESPACE_FIELD: usize = 65;
const NAME_NAMESPACE_MASK: u8 = 7;
const PARENT_HIGH_BYTE: usize = 7;
const MAX_MAPPING_WORDS: u8 = 8;
const TEST_RECORD_NUMBER: u64 = 7;
const IN_USE_FLAG: u16 = 1;
const DIRECTORY_FLAG: u16 = 2;
const CONTROL_BYTES: usize = 3;
const MAPPING_CONTROL: usize = 2;

const USA_OFFSET: usize = 48;
const RESIDENT_HEADER_BYTES: usize = 24;
const NONRESIDENT_HEADER_BYTES: usize = 64;
const FILE_NAME_HEADER_BYTES: usize = 66;
const ATTRIBUTE_END: u32 = 0xFFFF_FFFF;

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + size_of::<u16>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn le_u32(data: &[u8], offset: usize) -> u32 {
    let mut field = [0u8; size_of::<u32>()];
    field.copy_from_slice(&data[offset..offset + size_of::<u32>()]);
    u32::from_le_bytes(field)
}

fn le_u64(data: &[u8], offset: usize) -> u64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&data[offset..offset + RESIDENCY_FIELD]);
    u64::from_le_bytes(out)
}

/// Byte offset of the first attribute for a record of this geometry.
const fn attributes_offset_for(record_bytes: usize, sector_size: usize) -> usize {
    let update_sequence_length = record_bytes / sector_size + 1;
    (USA_OFFSET + update_sequence_length * UPDATE_SEQUENCE_WORD_BYTES)
        .next_multiple_of(ATTRIBUTE_ALIGNMENT)
}

/// A record whose header the validators accept, with `body` as its contents.
///
/// Only the fields that are pure conjunctions — signature, update-sequence geometry, `allocated_size`, the sequence/base-reference pairing rules and the `used_size` range — are repaired.
/// The flags word, the base reference's low bits, `used_size`'s exact value and the whole attribute region stay under fuzzer control.
fn base_record(record_bytes: usize, sector_size: usize, body: &[u8]) -> Vec<u8> {
    let mut record = vec![0u8; record_bytes];
    let taken = body.len().min(record_bytes);
    record[..taken].copy_from_slice(&body[..taken]);

    let update_sequence_length = record_bytes / sector_size + 1;
    let attributes_offset = attributes_offset_for(record_bytes, sector_size);

    record[..b"FILE".len()].copy_from_slice(b"FILE");
    put_u16(&mut record, USA_OFFSET_FIELD, USA_OFFSET as u16);
    put_u16(&mut record, USA_LENGTH_FIELD, update_sequence_length as u16);
    if le_u16(&record, SEQUENCE_OFFSET) == 0 {
        put_u16(&mut record, SEQUENCE_OFFSET, 1);
    }
    put_u16(
        &mut record,
        ATTRIBUTES_OFFSET_FIELD,
        attributes_offset as u16,
    );
    let span = record_bytes - attributes_offset;
    let used = attributes_offset + 1 + (le_u32(&record, USED_BYTES_FIELD) as usize % span);
    put_u32(&mut record, USED_BYTES_FIELD, used as u32);
    put_u32(&mut record, ALLOCATED_BYTES_FIELD, record_bytes as u32);
    let base_reference = le_u64(&record, BASE_REFERENCE_FIELD);
    if base_reference != 0 && base_reference >> SEQUENCE_SHIFT == 0 {
        put_u64(
            &mut record,
            BASE_REFERENCE_FIELD,
            base_reference | (1 << SEQUENCE_SHIFT),
        );
    }
    record
}

/// Stamp a resident attribute header over bytes the fuzzer already wrote; the value itself is left untouched.
fn resident_header(
    record: &mut [u8],
    offset: usize,
    type_id: u32,
    length: usize,
    value_length: usize,
    id: u16,
) {
    put_u32(record, offset, type_id);
    put_u32(record, offset + LENGTH_FIELD, length as u32);
    record[offset + RESIDENCY_FIELD] = 0;
    record[offset + NAME_UNITS_FIELD] = 0;
    put_u16(record, offset + NAME_OFFSET_FIELD, 0);
    put_u16(record, offset + FLAGS_FIELD, 0);
    put_u16(record, offset + ID_FIELD, id);
    put_u32(record, offset + RESIDENT_LENGTH_FIELD, value_length as u32);
    put_u16(
        record,
        offset + RESIDENT_OFFSET_FIELD,
        RESIDENT_HEADER_BYTES as u16,
    );
}

/// Stamp a non-resident attribute header; the VCN pair, the sizes and the mapping pairs after it all stay as the fuzzer wrote them.
fn nonresident_header(record: &mut [u8], offset: usize, type_id: u32, length: usize, id: u16) {
    put_u32(record, offset, type_id);
    put_u32(record, offset + LENGTH_FIELD, length as u32);
    record[offset + RESIDENCY_FIELD] = 1;
    record[offset + NAME_UNITS_FIELD] = 0;
    put_u16(record, offset + NAME_OFFSET_FIELD, 0);
    put_u16(record, offset + FLAGS_FIELD, 0);
    put_u16(record, offset + ID_FIELD, id);
    put_u16(
        record,
        offset + MAPPING_OFFSET_FIELD,
        NONRESIDENT_HEADER_BYTES as u16,
    );
}

/// Lay a complete, ordered attribute chain over the record's attribute region.
///
/// The longest chain this can produce is 376 bytes plus its eight-byte terminator margin, and the largest attribute offset any geometry yields is 72, so the chain fits even the smallest (512-byte) record.
fn frame_chain(record: &mut [u8], attributes_offset: usize, name_units: u8, mapping_words: u8) {
    let mut offset = attributes_offset;

    resident_header(
        record,
        offset,
        NtfsAttributeType::StandardInformation as u32,
        STANDARD_ATTRIBUTE_BYTES,
        STANDARD_VALUE_BYTES,
        1,
    );
    offset += STANDARD_ATTRIBUTE_BYTES;

    let units = usize::from(name_units % MAX_SHAPED_NAME_UNITS) + 1;
    let value_length = FILE_NAME_HEADER_BYTES + units * size_of::<u16>();
    let length = (RESIDENT_HEADER_BYTES + value_length).next_multiple_of(ATTRIBUTE_ALIGNMENT);
    resident_header(
        record,
        offset,
        NtfsAttributeType::FileName as u32,
        length,
        value_length,
        FILE_NAME_ID,
    );
    let value = offset + RESIDENT_HEADER_BYTES;
    record[value + NAME_LENGTH_FIELD] = units as u8;
    record[value + NAME_NAMESPACE_FIELD] &= NAME_NAMESPACE_MASK;
    record[value + PARENT_HIGH_BYTE] |= 1;
    offset += length;

    let length = NONRESIDENT_HEADER_BYTES
        + (usize::from(mapping_words % MAX_MAPPING_WORDS) + 1) * ATTRIBUTE_ALIGNMENT;
    nonresident_header(
        record,
        offset,
        NtfsAttributeType::Data as u32,
        length,
        DATA_ID,
    );
    offset += length;

    put_u32(record, offset, ATTRIBUTE_END);
    put_u32(
        record,
        USED_BYTES_FIELD,
        (offset + ATTRIBUTE_ALIGNMENT) as u32,
    );
}

/// Write the update-sequence number into every sector tail so `apply_fixup` takes its success path and swaps in the array's (fuzz-chosen) replacements.
fn stamp_sector_sentinels(record: &mut [u8], sector_size: usize) {
    let update_sequence_number = [record[USA_OFFSET], record[USA_OFFSET + 1]];
    for sector in 1..=record.len() / sector_size {
        let tail = sector * sector_size - UPDATE_SEQUENCE_WORD_BYTES;
        record[tail..tail + UPDATE_SEQUENCE_WORD_BYTES].copy_from_slice(&update_sequence_number);
    }
}

/// Run everything a scanner does with one buffer it believes is a record.
fn decode(record: &[u8], sector_size: usize) {
    let complete = attributes_complete(record);
    let valid = NtfsFile::is_valid(record, sector_size);
    let Some(file) = NtfsFile::parse(TEST_RECORD_NUMBER, record, sector_size) else {
        assert!(!valid);
        return;
    };
    assert!(valid);
    assert_eq!(
        file.reference_number() >> SEQUENCE_SHIFT,
        u64::from(file.header.sequence_value)
    );
    assert_eq!(
        file.reference_number() & RECORD_NUMBER_MASK,
        TEST_RECORD_NUMBER
    );
    assert_eq!(file.is_used(), file.header.flags & IN_USE_FLAG != 0);
    assert_eq!(file.is_directory(), file.header.flags & DIRECTORY_FLAG != 0);
    let mut attributes = Vec::new();
    let mut bytes = 0usize;
    file.attributes(|attribute| {
        assert_eq!(attribute.len(), attribute.data().len());
        assert_eq!(attribute.len(), attribute.header.length as usize);
        bytes = bytes
            .checked_add(attribute.len())
            .expect("attributes fit inside the record");
        attributes.push((attribute.header.type_id, attribute.data()));
        if let Some(value) = attribute.get_resident() {
            assert_eq!(attribute.header.is_non_resident, 0);
            assert_eq!(attribute.resident_value_length(), Some(value.len() as u32));
        }
        if let Some(header) = attribute.nonresident_header() {
            assert_eq!(attribute.header.is_non_resident, 1);
            assert!(usize::from(header.data_runs_offset) <= attribute.len());
        }
        if attribute.as_standard_info().is_some() {
            assert_eq!(
                attribute.header.type_id,
                NtfsAttributeType::StandardInformation as u32
            );
            assert!(attribute.get_resident().is_some());
        }
        if let Some(name) = attribute.as_name() {
            let encoded: Vec<u8> = name
                .to_utf16()
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            assert_eq!(encoded, name.utf16le);
            assert_eq!(
                encoded.len(),
                usize::from(name.header.name_length) * size_of::<u16>()
            );
        }
    });
    if complete {
        assert!(
            usize::from(file.header.attributes_offset) + bytes < file.header.used_size as usize
        );
    }
    for attribute_type in [
        NtfsAttributeType::StandardInformation,
        NtfsAttributeType::AttributeList,
        NtfsAttributeType::FileName,
        NtfsAttributeType::Data,
    ] {
        let expected = attributes
            .iter()
            .find(|(type_id, _)| *type_id == attribute_type as u32)
            .map(|(_, bytes)| *bytes);
        assert_eq!(
            file.get_attribute(attribute_type)
                .map(|attribute| attribute.data()),
            expected
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let control = |index: usize| data.get(index).copied().unwrap_or(0);
    let (record_bytes, sector_size) = GEOMETRIES[usize::from(control(0)) % GEOMETRIES.len()];
    let body = data.get(CONTROL_BYTES..).unwrap_or_default();

    decode(data, sector_size);
    let mut raw = data.to_vec();
    if apply_fixup(&mut raw, sector_size) {
        decode(&raw, sector_size);
    } else {
        assert_eq!(raw, data, "rejected fixup must preserve its input");
    }

    let mut record = base_record(record_bytes, sector_size, body);
    decode(&record, sector_size);
    let mut fixed = record.clone();
    stamp_sector_sentinels(&mut fixed, sector_size);
    if apply_fixup(&mut fixed, sector_size) {
        decode(&fixed, sector_size);
    }

    let attributes_offset = attributes_offset_for(record_bytes, sector_size);
    frame_chain(
        &mut record,
        attributes_offset,
        control(1),
        control(MAPPING_CONTROL),
    );
    decode(&record, sector_size);
    let mut fixed = record;
    stamp_sector_sentinels(&mut fixed, sector_size);
    if apply_fixup(&mut fixed, sector_size) {
        decode(&fixed, sector_size);
    }
});
