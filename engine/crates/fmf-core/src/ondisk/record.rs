//! Structural validation for one fixed-up NTFS file record.
//!
//! Index construction cannot accept an attribute prefix: a prefix is not a complete hard-link set, so validate the entire attribute chain first.

#![forbid(unsafe_code)]

use super::ntfs::{NtfsAttribute, RESIDENT_HEADER_BYTES};
use alloc::vec::Vec;

const ATTRIBUTE_ALIGNMENT: usize = 8;
const MIN_TERMINATED_ATTRIBUTE_BYTES: usize = RESIDENT_HEADER_BYTES + ATTRIBUTE_ALIGNMENT;
const ATTRIBUTE_TYPE_STEP: u32 = 0x10;
const ATTRIBUTES_OFFSET_FIELD: usize = 20;
const USED_BYTES_FIELD: usize = 24;
const ALLOCATED_BYTES_FIELD: usize = 28;

const FILE_RECORD_HEADER_BYTES: usize = 42;
const ATTRIBUTE_END: u32 = 0xFFFF_FFFF;
const MAX_ATTRIBUTE_TYPE: u32 = 0x100;
const STANDARD_INFORMATION_ATTRIBUTE: u32 = 0x10;
const ATTRIBUTE_LIST_ATTRIBUTE: u32 = 0x20;
const FILE_NAME_ATTRIBUTE: u32 = 0x30;
const INLINE_HIGH_IDS: usize = 4;

#[derive(Default)]
struct AttributeIds {
    low: u128,
    high: [u16; INLINE_HIGH_IDS],
    high_len: usize,
    overflow: Vec<u16>,
}

impl AttributeIds {
    fn insert(&mut self, id: u16) -> bool {
        if id < u128::BITS as u16 {
            let bit = 1u128 << id;
            if self.low & bit != 0 {
                return false;
            }
            self.low |= bit;
            return true;
        }
        if self.high[..self.high_len].contains(&id) || self.overflow.contains(&id) {
            return false;
        }
        if self.high_len < self.high.len() {
            self.high[self.high_len] = id;
            self.high_len += 1;
        } else {
            self.overflow.push(id);
        }
        true
    }
}

const fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

const fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + size_of::<u16>()],
        data[offset + size_of::<u32>() - 1],
    ])
}

/// True only when the complete attribute chain is bounded, aligned, and terminated.
///
/// Resident values, attribute names, and non-resident mapping-pair offsets must stay inside their owning attribute.
#[must_use]
pub fn attributes_complete(record: &[u8]) -> bool {
    if record.get(..FILE_RECORD_HEADER_BYTES).is_none() {
        return false;
    }
    let used = le_u32(record, USED_BYTES_FIELD) as usize;
    let allocated = le_u32(record, ALLOCATED_BYTES_FIELD) as usize;
    let offset = usize::from(le_u16(record, ATTRIBUTES_OFFSET_FIELD));
    if allocated != record.len()
        || !offset.is_multiple_of(ATTRIBUTE_ALIGNMENT)
        || record
            .get(..offset)
            .and_then(|prefix| prefix.get(..FILE_RECORD_HEADER_BYTES))
            .is_none()
    {
        return false;
    }
    let Some(mut remaining) = record.get(offset..used) else {
        return false;
    };
    let mut ids = AttributeIds::default();
    let mut previous_type = 0u32;
    loop {
        if remaining.get(..ATTRIBUTE_ALIGNMENT).is_none() {
            return false;
        }
        let type_id = le_u32(remaining, 0);
        if type_id == ATTRIBUTE_END {
            return true;
        }
        if remaining.len() < MIN_TERMINATED_ATTRIBUTE_BYTES {
            return false;
        }
        if type_id == 0
            || type_id > MAX_ATTRIBUTE_TYPE
            || !type_id.is_multiple_of(ATTRIBUTE_TYPE_STEP)
            || type_id < previous_type
        {
            return false;
        }
        let Some(attribute) = NtfsAttribute::parse(remaining) else {
            return false;
        };
        let length = attribute.len();
        let Some(rest) = remaining.get(length..) else {
            return false;
        };
        if rest.len() < ATTRIBUTE_ALIGNMENT {
            return false;
        }
        if !length.is_multiple_of(ATTRIBUTE_ALIGNMENT) || !ids.insert(attribute.header.id) {
            return false;
        }
        previous_type = type_id;
        let header = attribute.header;
        if matches!(
            type_id,
            STANDARD_INFORMATION_ATTRIBUTE | ATTRIBUTE_LIST_ATTRIBUTE | FILE_NAME_ATTRIBUTE
        ) && (header.name_length != 0 || header.flags != 0)
        {
            return false;
        }
        let name_end = (header.name_length > 0).then(|| {
            usize::from(header.name_offset) + usize::from(header.name_length) * size_of::<u16>()
        });
        if header.is_non_resident == 0 {
            let Some(value) = attribute.resident_value_range() else {
                return false;
            };
            if name_end.is_some_and(|end| value.start < end) {
                return false;
            }
        } else {
            if matches!(
                type_id,
                STANDARD_INFORMATION_ATTRIBUTE | FILE_NAME_ATTRIBUTE
            ) {
                return false;
            }
            let Some(nonresident) = attribute.nonresident_header() else {
                return false;
            };
            let mapping_offset = usize::from(nonresident.data_runs_offset);
            if !mapping_offset.is_multiple_of(ATTRIBUTE_ALIGNMENT)
                || mapping_offset >= length
                || name_end.is_some_and(|end| mapping_offset < end)
            {
                return false;
            }
        }
        remaining = rest;
    }
}

#[cfg(kani)]
#[path = "record_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ATTRIBUTE_NAME_UNITS_FIELD: usize = 9;
    const RESIDENT_OFFSET_FIELD: usize = 20;

    fn put_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn resident_record() -> Vec<u8> {
        let mut record = vec![0u8; 128];
        let allocated = record.len() as u32;
        put_u16(&mut record, 20, 48);
        put_u32(&mut record, 24, 88);
        put_u32(&mut record, 28, allocated);
        put_u32(&mut record, 48, FILE_NAME_ATTRIBUTE);
        put_u32(&mut record, 52, 32);
        put_u32(&mut record, 64, 4);
        put_u16(&mut record, 68, 24);
        put_u32(&mut record, 80, ATTRIBUTE_END);
        record
    }

    fn two_resident_attributes(first_type: u32, second_type: u32) -> Vec<u8> {
        let mut record = vec![0u8; 160];
        put_u16(&mut record, 20, 48);
        put_u32(&mut record, 24, 120);
        put_u32(&mut record, 28, 160);
        for (offset, type_id, id) in [(48usize, first_type, 7u16), (80, second_type, 8)] {
            put_u32(&mut record, offset, type_id);
            put_u32(&mut record, offset + 4, 32);
            put_u16(&mut record, offset + 14, id);
            put_u32(&mut record, offset + 16, 4);
            put_u16(&mut record, offset + 20, 24);
        }
        put_u32(&mut record, 112, ATTRIBUTE_END);
        record
    }

    fn named_resident_attribute(type_id: u32) -> Vec<u8> {
        let mut record = vec![0u8; 128];
        put_u16(&mut record, 20, 48);
        put_u32(&mut record, 24, 96);
        put_u32(&mut record, 28, 128);
        put_u32(&mut record, 48, type_id);
        put_u32(&mut record, 52, 40);
        record[57] = 1;
        put_u16(&mut record, 58, 24);
        put_u16(&mut record, 62, 1);
        put_u32(&mut record, 64, 4);
        put_u16(&mut record, 68, 32);
        put_u16(&mut record, 72, b'x' as u16);
        put_u32(&mut record, 88, ATTRIBUTE_END);
        record
    }

    #[test]
    fn first_high_attribute_ids_stay_inline() {
        let mut ids = AttributeIds::default();
        for id in [128, 255, 32_768, u16::MAX] {
            assert!(ids.insert(id));
            assert_eq!(ids.overflow.capacity(), 0);
        }
        assert_eq!(ids.high_len, INLINE_HIGH_IDS);
        assert_eq!(ids.high, [128, 255, 32_768, u16::MAX]);
    }

    #[test]
    fn duplicate_ids_preserve_membership_across_every_storage_region() {
        let mut ids = AttributeIds::default();
        let input = [0, 127, 128, 129, 130, 131, 132, 133, u16::MAX];
        for id in input {
            assert!(ids.insert(id));
        }
        let before = (ids.low, ids.high, ids.high_len, ids.overflow.clone());
        for id in input {
            assert!(!ids.insert(id));
            assert_eq!(
                (ids.low, ids.high, ids.high_len, &ids.overflow),
                (before.0, before.1, before.2, &before.3)
            );
        }
        assert!(ids.insert(1));
        assert!(ids.insert(134));
    }

    #[test]
    fn accepts_a_complete_resident_chain() {
        assert!(attributes_complete(&resident_record()));
    }

    #[test]
    fn a_minimal_resident_attribute_requires_every_terminator_byte() {
        for tail in 0..=24 {
            let mut record = resident_record();
            record.resize(72 + tail, 0);
            let bytes = record.len() as u32;
            put_u32(&mut record, USED_BYTES_FIELD, bytes);
            put_u32(&mut record, ALLOCATED_BYTES_FIELD, bytes);
            put_u32(&mut record, 52, 24);
            put_u32(&mut record, 64, 0);
            put_u16(&mut record, RESIDENT_OFFSET_FIELD + 48, 24);
            let marker_bytes = tail.min(size_of::<u32>());
            record[72..72 + marker_bytes].fill(0xff);
            assert_eq!(attributes_complete(&record), tail >= 8, "tail {tail}");
        }
    }

    #[test]
    fn complete_chains_preserve_both_attribute_offset_bytes() {
        let original = resident_record();
        for start in [48u16, 304, 560, 816] {
            let mut record = vec![0; 1024];
            record[..FILE_RECORD_HEADER_BYTES]
                .copy_from_slice(&original[..FILE_RECORD_HEADER_BYTES]);
            let offset = usize::from(start);
            record[offset..offset + 40].copy_from_slice(&original[48..88]);
            put_u16(&mut record, ATTRIBUTES_OFFSET_FIELD, start);
            put_u32(&mut record, USED_BYTES_FIELD, u32::from(start) + 40);
            put_u32(&mut record, ALLOCATED_BYTES_FIELD, 1024);
            assert!(attributes_complete(&record), "attribute offset {start}");
        }
    }

    #[test]
    fn complete_chains_accept_the_maximum_attribute_type() {
        let mut record = resident_record();
        put_u32(&mut record, 48, MAX_ATTRIBUTE_TYPE);
        assert!(attributes_complete(&record));
        put_u32(&mut record, 48, MAX_ATTRIBUTE_TYPE + ATTRIBUTE_TYPE_STEP);
        assert!(!attributes_complete(&record));
    }

    #[test]
    fn rejects_a_misaligned_complete_attribute_chain() {
        let valid = resident_record();
        assert!(attributes_complete(&valid));
        let start = usize::from(le_u16(&valid, ATTRIBUTES_OFFSET_FIELD));
        let used = le_u32(&valid, USED_BYTES_FIELD) as usize;
        for shift in 1..ATTRIBUTE_ALIGNMENT {
            let mut shifted = valid.clone();
            shifted.copy_within(start..used, start + shift);
            put_u16(
                &mut shifted,
                ATTRIBUTES_OFFSET_FIELD,
                (start + shift) as u16,
            );
            put_u32(&mut shifted, USED_BYTES_FIELD, (used + shift) as u32);
            assert!(!attributes_complete(&shifted), "shift {shift}");
        }
    }

    #[test]
    fn attribute_ids_are_unique_across_both_storage_ranges() {
        let mut ids = AttributeIds::default();
        for id in [0, 1, 126, 127, 128, 129, 255, 256, u16::MAX] {
            assert!(ids.insert(id), "first id {id}");
            assert!(!ids.insert(id), "duplicate id {id}");
        }
    }

    #[test]
    fn resident_name_and_payload_regions_are_bounded_and_ordered() {
        for name_units in [0u8, 1, 4, 5, 8] {
            for value_offset in [0u16, 23, 24, 25, 26, 31, 32, 36, 37, 40, u16::MAX] {
                let mut record = named_resident_attribute(0x80);
                record[48 + ATTRIBUTE_NAME_UNITS_FIELD] = name_units;
                put_u16(&mut record, 48 + RESIDENT_OFFSET_FIELD, value_offset);
                let value_start = usize::from(value_offset);
                let name_end = 24 + usize::from(name_units) * 2;
                let expected = (24..=36).contains(&value_start)
                    && (name_units == 0 || name_end <= value_start);
                assert_eq!(
                    attributes_complete(&record),
                    expected,
                    "name units {name_units}, value offset {value_offset}"
                );
            }
        }
    }

    #[test]
    fn nonresident_names_precede_aligned_mapping_pairs_inside_the_attribute() {
        for name_units in [0u8, 1, 4, 5] {
            for name_offset in [0u16, 63, 64, 65, 66, 88, 95, u16::MAX] {
                for mapping_offset in [0u16, 63, 64, 65, 72, 88, 96, 104, u16::MAX] {
                    let mut record = vec![0; 160];
                    put_u16(&mut record, 20, 48);
                    put_u32(&mut record, 24, 152);
                    put_u32(&mut record, 28, 160);
                    put_u32(&mut record, 48, 0x80);
                    put_u32(&mut record, 52, 96);
                    record[56] = 1;
                    record[57] = name_units;
                    put_u16(&mut record, 58, name_offset);
                    put_u16(&mut record, 80, mapping_offset);
                    put_u32(&mut record, 144, ATTRIBUTE_END);
                    let name_end = usize::from(name_offset) + usize::from(name_units) * 2;
                    let expected = (64..96).contains(&mapping_offset)
                        && mapping_offset.is_multiple_of(8)
                        && (name_units == 0
                            || (name_offset >= 64 && name_end <= usize::from(mapping_offset)));
                    assert_eq!(
                        attributes_complete(&record),
                        expected,
                        "name {name_offset}+{name_units}, mapping {mapping_offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn rejects_an_unterminated_or_truncated_attribute_chain() {
        let mut unterminated = resident_record();
        put_u32(&mut unterminated, 80, 0);
        assert!(!attributes_complete(&unterminated));

        let mut escaping_value = resident_record();
        put_u32(&mut escaping_value, 64, 9);
        assert!(!attributes_complete(&escaping_value));

        let mut short_end = resident_record();
        put_u32(&mut short_end, 24, 84);
        assert!(!attributes_complete(&short_end));
    }

    #[test]
    fn rejects_nonresident_file_names_and_invalid_mapping_offsets() {
        let mut nonresident_name = resident_record();
        nonresident_name[56] = 1;
        put_u32(&mut nonresident_name, 52, 64);
        put_u16(&mut nonresident_name, 80, 64);
        put_u32(&mut nonresident_name, 112, ATTRIBUTE_END);
        put_u32(&mut nonresident_name, 24, 116);
        assert!(!attributes_complete(&nonresident_name));
    }

    #[test]
    fn rejects_allocated_size_mismatch_and_duplicate_attribute_ids() {
        let mut wrong_allocation = resident_record();
        put_u32(&mut wrong_allocation, 28, 64);
        assert!(!attributes_complete(&wrong_allocation));

        let mut duplicate = vec![0u8; 160];
        put_u16(&mut duplicate, 20, 48);
        put_u32(&mut duplicate, 24, 120);
        put_u32(&mut duplicate, 28, 160);
        for offset in [48usize, 80] {
            put_u32(&mut duplicate, offset, FILE_NAME_ATTRIBUTE);
            put_u32(&mut duplicate, offset + 4, 32);
            put_u16(&mut duplicate, offset + 14, 7);
            put_u32(&mut duplicate, offset + 16, 4);
            put_u16(&mut duplicate, offset + 20, 24);
        }
        put_u32(&mut duplicate, 112, ATTRIBUTE_END);
        assert!(!attributes_complete(&duplicate));
    }

    #[test]
    fn rejects_invalid_or_descending_attribute_types() {
        for invalid in [0, 0x11, MAX_ATTRIBUTE_TYPE + 0x10] {
            let mut record = resident_record();
            put_u32(&mut record, 48, invalid);
            assert!(!attributes_complete(&record));
        }
        assert!(!attributes_complete(&two_resident_attributes(
            0x80,
            FILE_NAME_ATTRIBUTE,
        )));
    }

    #[test]
    fn equal_types_remain_valid_for_hard_links_and_split_extents() {
        assert!(attributes_complete(&two_resident_attributes(
            FILE_NAME_ATTRIBUTE,
            FILE_NAME_ATTRIBUTE,
        )));
        assert!(attributes_complete(&two_resident_attributes(
            ATTRIBUTE_LIST_ATTRIBUTE,
            ATTRIBUTE_LIST_ATTRIBUTE,
        )));
    }

    #[test]
    fn relevant_system_attributes_are_unnamed_unflagged_and_resident_when_required() {
        for type_id in [
            STANDARD_INFORMATION_ATTRIBUTE,
            ATTRIBUTE_LIST_ATTRIBUTE,
            FILE_NAME_ATTRIBUTE,
        ] {
            assert!(!attributes_complete(&named_resident_attribute(type_id)));

            let mut flagged = resident_record();
            put_u32(&mut flagged, 48, type_id);
            put_u16(&mut flagged, 60, 1);
            assert!(!attributes_complete(&flagged));
        }

        let mut nonresident_standard = resident_record();
        put_u32(
            &mut nonresident_standard,
            48,
            STANDARD_INFORMATION_ATTRIBUTE,
        );
        nonresident_standard[56] = 1;
        put_u32(&mut nonresident_standard, 52, 64);
        put_u16(&mut nonresident_standard, 80, 64);
        put_u32(&mut nonresident_standard, 112, ATTRIBUTE_END);
        put_u32(&mut nonresident_standard, 24, 120);
        assert!(!attributes_complete(&nonresident_standard));
    }

    #[test]
    fn nonresident_mapping_offset_must_leave_a_terminator_byte() {
        let mut record = vec![0u8; 160];
        put_u16(&mut record, 20, 48);
        put_u32(&mut record, 24, 120);
        put_u32(&mut record, 28, 160);
        put_u32(&mut record, 48, 0x80);
        put_u32(&mut record, 52, 64);
        record[56] = 1;
        put_u16(&mut record, 80, 64);
        put_u32(&mut record, 112, ATTRIBUTE_END);
        assert!(!attributes_complete(&record));
    }

    proptest! {
        #[test]
        fn id_membership_matches_an_independent_set(
            input in proptest::collection::vec(any::<u16>(), 0..96),
        ) {
            let mut ids = AttributeIds::default();
            let mut expected = alloc::collections::BTreeSet::new();
            for id in input {
                prop_assert_eq!(ids.insert(id), expected.insert(id));
            }
        }

        #[test]
        fn arbitrary_record_bytes_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
        ) {
            let _ = attributes_complete(&bytes);
        }
    }
}
