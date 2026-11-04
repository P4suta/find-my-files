//! Structural validation for one fixed-up NTFS file record.
//!
//! Index construction cannot accept an attribute prefix: a prefix is not a complete hard-link set, so validate the entire attribute chain first.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

const ATTRIBUTE_ALIGNMENT: usize = 8;
const ATTRIBUTE_TYPE_STEP: u32 = 0x10;
const ATTRIBUTES_OFFSET_FIELD: usize = 20;
const USED_BYTES_FIELD: usize = 24;
const ALLOCATED_BYTES_FIELD: usize = 28;
const ATTRIBUTE_LENGTH_FIELD: usize = 4;
const ATTRIBUTE_RESIDENCY_FIELD: usize = 8;
const ATTRIBUTE_NAME_UNITS_FIELD: usize = 9;
const ATTRIBUTE_NAME_OFFSET_FIELD: usize = 10;
const ATTRIBUTE_FLAGS_FIELD: usize = 12;
const ATTRIBUTE_ID_FIELD: usize = 14;
const RESIDENT_LENGTH_FIELD: usize = 16;
const RESIDENT_OFFSET_FIELD: usize = 20;
const MAPPING_PAIRS_OFFSET_FIELD: usize = 32;

const FILE_RECORD_HEADER_BYTES: usize = 42;
const ATTRIBUTE_HEADER_BYTES: usize = 16;
const RESIDENT_HEADER_BYTES: usize = 24;
const NONRESIDENT_HEADER_BYTES: usize = 64;
const ATTRIBUTE_END: u32 = 0xFFFF_FFFF;
const MAX_ATTRIBUTE_TYPE: u32 = 0x100;
const STANDARD_INFORMATION_ATTRIBUTE: u32 = 0x10;
const ATTRIBUTE_LIST_ATTRIBUTE: u32 = 0x20;
const FILE_NAME_ATTRIBUTE: u32 = 0x30;

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

fn unique_attribute_id(id: u16, low_ids: &mut u128, high_ids: &mut Vec<u16>) -> bool {
    if id < u128::BITS as u16 {
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

/// True only when the complete attribute chain is bounded, aligned, and terminated.
///
/// Resident values, attribute names, and non-resident mapping-pair offsets must stay inside their owning attribute.
#[must_use]
pub fn attributes_complete(record: &[u8]) -> bool {
    if record.len() < FILE_RECORD_HEADER_BYTES {
        return false;
    }
    let used = le_u32(record, USED_BYTES_FIELD) as usize;
    let allocated = le_u32(record, ALLOCATED_BYTES_FIELD) as usize;
    let mut offset = le_u16(record, ATTRIBUTES_OFFSET_FIELD) as usize;
    if allocated != record.len()
        || used > allocated
        || offset < FILE_RECORD_HEADER_BYTES
        || !offset.is_multiple_of(ATTRIBUTE_ALIGNMENT)
        || offset >= used
    {
        return false;
    }
    let mut low_ids = 0u128;
    let mut high_ids = Vec::new();
    let mut previous_type = 0u32;

    loop {
        let Some(type_end) = offset.checked_add(ATTRIBUTE_ALIGNMENT) else {
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
            || !type_id.is_multiple_of(ATTRIBUTE_TYPE_STEP)
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
        let length = le_u32(record, offset + ATTRIBUTE_LENGTH_FIELD) as usize;
        let Some(attribute_end) = offset.checked_add(length) else {
            return false;
        };
        if length < ATTRIBUTE_HEADER_BYTES
            || !length.is_multiple_of(ATTRIBUTE_ALIGNMENT)
            || attribute_end > used
        {
            return false;
        }
        if !unique_attribute_id(
            le_u16(record, offset + ATTRIBUTE_ID_FIELD),
            &mut low_ids,
            &mut high_ids,
        ) {
            return false;
        }

        let name_units = record[offset + ATTRIBUTE_NAME_UNITS_FIELD] as usize;
        let flags = le_u16(record, offset + ATTRIBUTE_FLAGS_FIELD);
        if matches!(
            type_id,
            STANDARD_INFORMATION_ATTRIBUTE | ATTRIBUTE_LIST_ATTRIBUTE | FILE_NAME_ATTRIBUTE
        ) && (name_units != 0 || flags != 0)
        {
            return false;
        }
        let name_range = if name_units > 0 {
            let name_offset = le_u16(record, offset + ATTRIBUTE_NAME_OFFSET_FIELD) as usize;
            let Some(name_bytes) = name_units.checked_mul(size_of::<u16>()) else {
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

        match record[offset + ATTRIBUTE_RESIDENCY_FIELD] {
            0 => {
                if length < RESIDENT_HEADER_BYTES {
                    return false;
                }
                let value_length = le_u32(record, offset + RESIDENT_LENGTH_FIELD) as usize;
                let value_offset = le_u16(record, offset + RESIDENT_OFFSET_FIELD) as usize;
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
                let mapping_offset = le_u16(record, offset + MAPPING_PAIRS_OFFSET_FIELD) as usize;
                if mapping_offset < NONRESIDENT_HEADER_BYTES
                    || !mapping_offset.is_multiple_of(ATTRIBUTE_ALIGNMENT)
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
