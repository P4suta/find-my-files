//! Structural validation for one fixed-up NTFS file record.
//!
//! `ntfs-reader` deliberately stops its callback iterator at the first bad
//! attribute. Index construction needs a stronger contract: a prefix is not a
//! complete hard-link set, so validate the entire attribute chain first.

const FILE_RECORD_HEADER_BYTES: usize = 42;
const ATTRIBUTE_HEADER_BYTES: usize = 16;
const RESIDENT_HEADER_BYTES: usize = 24;
const NONRESIDENT_HEADER_BYTES: usize = 64;
const ATTRIBUTE_END: u32 = 0xFFFF_FFFF;
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

/// True only when the complete attribute chain is bounded, aligned, and
/// terminated. Resident values, attribute names, and non-resident mapping-pair
/// offsets must stay inside their owning attribute.
pub fn attributes_complete(record: &[u8]) -> bool {
    if record.len() < FILE_RECORD_HEADER_BYTES {
        return false;
    }
    let used = le_u32(record, 24) as usize;
    let mut offset = le_u16(record, 20) as usize;
    if used > record.len() || offset < FILE_RECORD_HEADER_BYTES || offset >= used {
        return false;
    }

    loop {
        let Some(type_end) = offset.checked_add(size_of::<u32>()) else {
            return false;
        };
        if type_end > used {
            return false;
        }
        let type_id = le_u32(record, offset);
        if type_id == ATTRIBUTE_END {
            return true;
        }
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

        let name_units = record[offset + 9] as usize;
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
                if type_id == FILE_NAME_ATTRIBUTE || length < NONRESIDENT_HEADER_BYTES {
                    return false;
                }
                let mapping_offset = le_u16(record, offset + 32) as usize;
                if mapping_offset < NONRESIDENT_HEADER_BYTES
                    || mapping_offset > length
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

    fn put_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn resident_record() -> Vec<u8> {
        let mut record = vec![0u8; 128];
        put_u16(&mut record, 20, 48);
        put_u32(&mut record, 24, 88);
        put_u32(&mut record, 48, FILE_NAME_ATTRIBUTE);
        put_u32(&mut record, 52, 32);
        put_u32(&mut record, 64, 4);
        put_u16(&mut record, 68, 24);
        put_u32(&mut record, 80, ATTRIBUTE_END);
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
}
