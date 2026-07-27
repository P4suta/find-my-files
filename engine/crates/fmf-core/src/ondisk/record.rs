//! Structural validation for one fixed-up NTFS file record.
//!
//! Index construction cannot accept an attribute prefix: a prefix is not a
//! complete hard-link set, so validate the entire attribute chain first.

#![forbid(unsafe_code)]

const FILE_RECORD_HEADER_BYTES: usize = 42;
const ATTRIBUTE_HEADER_BYTES: usize = 16;
const RESIDENT_HEADER_BYTES: usize = 24;
const NONRESIDENT_HEADER_BYTES: usize = 64;
const ATTRIBUTE_END: u32 = 0xFFFF_FFFF;
const MAX_ATTRIBUTE_TYPE: u32 = 0x100;
const STANDARD_INFORMATION_ATTRIBUTE: u32 = 0x10;
const ATTRIBUTE_LIST_ATTRIBUTE: u32 = 0x20;
const FILE_NAME_ATTRIBUTE: u32 = 0x30;

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

fn unique_attribute_id(id: u16, low_ids: &mut u128, high_ids: &mut Vec<u16>) -> bool {
    if id < 128 {
        let bit = 1u128 << id;
        if *low_ids & bit != 0 {
            return false;
        }
        *low_ids |= bit;
        true
    } else if high_ids.contains(&id) {
        false
    } else {
        high_ids.push(id);
        true
    }
}

/// True only when the complete attribute chain is bounded, aligned, and
/// terminated.
///
/// Resident values, attribute names, and non-resident mapping-pair offsets
/// must stay inside their owning attribute.
#[must_use]
pub fn attributes_complete(record: &[u8]) -> bool {
    if record.len() < FILE_RECORD_HEADER_BYTES {
        return false;
    }
    let used = le_u32(record, 24) as usize;
    let allocated = le_u32(record, 28) as usize;
    let mut offset = le_u16(record, 20) as usize;
    if allocated != record.len()
        || used > allocated
        || offset < FILE_RECORD_HEADER_BYTES
        || !offset.is_multiple_of(8)
        || offset >= used
    {
        return false;
    }
    let mut low_ids = 0u128;
    let mut high_ids = Vec::new();
    let mut previous_type = 0u32;

    loop {
        let Some(type_end) = offset.checked_add(8) else {
            return false;
        };
        if type_end > used {
            return false;
        }
        let type_id = le_u32(record, offset);
        if type_id == ATTRIBUTE_END {
            return true;
        }
        if type_id == 0
            || type_id > MAX_ATTRIBUTE_TYPE
            || !type_id.is_multiple_of(0x10)
            || type_id < previous_type
        {
            return false;
        }
        previous_type = type_id;
        let Some(header_end) = offset.checked_add(ATTRIBUTE_HEADER_BYTES) else {
            return false;
        };
        if header_end > used {
            return false;
        }
        let length = le_u32(record, offset + 4) as usize;
        let Some(attribute_end) = offset.checked_add(length) else {
            return false;
        };
        if length < ATTRIBUTE_HEADER_BYTES || !length.is_multiple_of(8) || attribute_end > used {
            return false;
        }
        if !unique_attribute_id(le_u16(record, offset + 14), &mut low_ids, &mut high_ids) {
            return false;
        }

        let name_units = record[offset + 9] as usize;
        let flags = le_u16(record, offset + 12);
        if matches!(
            type_id,
            STANDARD_INFORMATION_ATTRIBUTE | ATTRIBUTE_LIST_ATTRIBUTE | FILE_NAME_ATTRIBUTE
        ) && (name_units != 0 || flags != 0)
        {
            return false;
        }
        let name_range = if name_units > 0 {
            let name_offset = le_u16(record, offset + 10) as usize;
            let Some(name_bytes) = name_units.checked_mul(2) else {
                return false;
            };
            let Some(name_end) = name_offset.checked_add(name_bytes) else {
                return false;
            };
            if name_end > length {
                return false;
            }
            Some((name_offset, name_end))
        } else {
            None
        };

        match record[offset + 8] {
            0 => {
                if length < RESIDENT_HEADER_BYTES {
                    return false;
                }
                let value_length = le_u32(record, offset + 16) as usize;
                let value_offset = le_u16(record, offset + 20) as usize;
                if value_offset < RESIDENT_HEADER_BYTES
                    || name_range.is_some_and(|(start, end)| {
                        start < RESIDENT_HEADER_BYTES || value_offset < end
                    })
                    || value_offset
                        .checked_add(value_length)
                        .is_none_or(|end| end > length)
                {
                    return false;
                }
            }
            1 => {
                if matches!(
                    type_id,
                    STANDARD_INFORMATION_ATTRIBUTE | FILE_NAME_ATTRIBUTE
                ) || length < NONRESIDENT_HEADER_BYTES
                {
                    return false;
                }
                let mapping_offset = le_u16(record, offset + 32) as usize;
                if mapping_offset < NONRESIDENT_HEADER_BYTES
                    || !mapping_offset.is_multiple_of(8)
                    || mapping_offset >= length
                    || name_range.is_some_and(|(start, end)| {
                        start < NONRESIDENT_HEADER_BYTES || mapping_offset < end
                    })
                {
                    return false;
                }
            }
            _ => return false,
        }
        offset = attribute_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
    fn accepts_a_complete_resident_chain() {
        assert!(attributes_complete(&resident_record()));
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
        // FILE_NAME legitimately repeats for hard links. ATTRIBUTE_LIST can
        // also describe split extents despite the Microsoft prose saying
        // there is at most one unnamed attribute per type; identity/VCN
        // uniqueness is therefore enforced by the list parser, not by making
        // the record's type order artificially strict.
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
        fn arbitrary_record_bytes_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
        ) {
            let _ = attributes_complete(&bytes);
        }
    }
}
