use super::{
    ALLOCATED_BYTES_FIELD, ATTRIBUTE_END, ATTRIBUTES_OFFSET_FIELD, USED_BYTES_FIELD,
    attributes_complete, le_u16, le_u32,
};
use crate::ondisk::ntfs::NtfsAttribute;

#[path = "record_admission_reference.rs"]
mod admission_reference;

const PROOF_BYTES: usize = 128;
const PROOF_ATTRIBUTES: usize = 4;

#[kani::proof]
#[kani::unwind(9)]
fn accepted_chains_have_bounded_ordered_unique_attributes() {
    let bytes: [u8; PROOF_BYTES] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= bytes.len());
    let record = &bytes[..length];
    let accepted = attributes_complete(record);
    assert_eq!(accepted, admission_reference::attributes_complete(record));
    let mut count = 0;
    let mut nonresident = false;
    if accepted {
        assert!(record.len() >= 42);
        let used = le_u32(record, USED_BYTES_FIELD) as usize;
        let allocated = le_u32(record, ALLOCATED_BYTES_FIELD) as usize;
        let start = usize::from(le_u16(record, ATTRIBUTES_OFFSET_FIELD));
        assert_eq!(allocated, record.len());
        assert!(start >= 42 && start < used && used <= allocated);
        assert!(start.is_multiple_of(8));
        let mut remaining = &record[start..used];
        let mut ids = [0u16; PROOF_ATTRIBUTES];
        let mut previous_type = 0;
        loop {
            assert!(remaining.len() >= 8);
            if le_u32(remaining, 0) == ATTRIBUTE_END {
                break;
            }
            let attribute = NtfsAttribute::parse(remaining);
            assert!(attribute.is_some());
            let Some(attribute) = attribute else {
                return;
            };
            assert!(attribute.len() >= 24 && attribute.len().is_multiple_of(8));
            assert!(attribute.header.type_id >= previous_type);
            assert!((0x10..=0x100).contains(&attribute.header.type_id));
            assert!(attribute.header.type_id.is_multiple_of(0x10));
            previous_type = attribute.header.type_id;
            assert!(count < ids.len());
            assert!(!ids[..count].contains(&attribute.header.id));
            ids[count] = attribute.header.id;
            count += 1;
            nonresident |= attribute.header.is_non_resident == 1;
            if matches!(attribute.header.type_id, 0x10 | 0x20 | 0x30) {
                assert_eq!(attribute.header.name_length, 0);
                assert_eq!(attribute.header.flags, 0);
            }
            let rest = remaining.get(attribute.len()..);
            assert!(rest.is_some());
            let Some(rest) = rest else {
                return;
            };
            assert!(rest.len() < remaining.len());
            remaining = rest;
        }
    }
    kani::cover!(accepted);
    kani::cover!(!accepted);
    kani::cover!(accepted && count > 1);
    kani::cover!(accepted && nonresident);
}
