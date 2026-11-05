use super::{
    ALLOCATED_BYTES_FIELD, ATTRIBUTE_END, ATTRIBUTES_OFFSET_FIELD, USED_BYTES_FIELD,
    attributes_complete, le_u16, le_u32,
};
use crate::ondisk::ntfs::NtfsAttribute;

#[path = "record_admission_reference.rs"]
mod admission_reference;

const PROOF_BYTES: usize = 128;
const _: () = assert!(PROOF_BYTES <= u8::MAX as usize);
const PROOF_ATTRIBUTES: usize = 4;
const REFERENCE_FILE_HEADER_BYTES: usize = 42;
const REFERENCE_RESIDENT_HEADER_BYTES: usize = 24;
const REFERENCE_ALIGNMENT: usize = 8;
const REFERENCE_ATTRIBUTE_TYPE_STEP: u32 = 0x10;
const REFERENCE_MAX_ATTRIBUTE_TYPE: u32 = 0x100;
const REFERENCE_STANDARD_INFORMATION: u32 = 0x10;
const REFERENCE_ATTRIBUTE_LIST: u32 = 0x20;
const REFERENCE_FILE_NAME: u32 = 0x30;
const TERMINATOR_STARTS: [u16; 3] = [104, 112, 120];
const ATTRIBUTE_OFFSET_48: u16 = 48;
const ATTRIBUTE_OFFSET_56: u16 = 56;
const ATTRIBUTE_OFFSET_64: u16 = 64;
const ATTRIBUTE_OFFSET_72: u16 = 72;
const ATTRIBUTE_OFFSET_80: u16 = 80;
const ATTRIBUTE_OFFSET_88: u16 = 88;
const ATTRIBUTE_OFFSET_96: u16 = 96;

const _: () = {
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_48 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 2 <= 5
    );
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_64 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 2 <= 4
    );
    assert!(TERMINATOR_STARTS.len() < 4);
};

struct ChainChecks {
    accepted: bool,
    reference: bool,
    count: usize,
    nonresident: bool,
}

fn check_chain_at<const START: u16>() -> ChainChecks {
    let bytes: [u8; PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    kani::assume(le_u16(&bytes, ATTRIBUTES_OFFSET_FIELD) == START);
    let record = &bytes[..length];
    let accepted = attributes_complete(record);
    let reference = admission_reference::attributes_complete(record);
    let mut count = 0;
    let mut nonresident = false;
    if accepted {
        assert!(record.len() >= REFERENCE_FILE_HEADER_BYTES);
        let used = le_u32(record, USED_BYTES_FIELD) as usize;
        let allocated = le_u32(record, ALLOCATED_BYTES_FIELD) as usize;
        let start = usize::from(le_u16(record, ATTRIBUTES_OFFSET_FIELD));
        assert_eq!(allocated, record.len());
        assert!(start >= REFERENCE_FILE_HEADER_BYTES && start < used && used <= allocated);
        assert!(start.is_multiple_of(REFERENCE_ALIGNMENT));
        let mut remaining = &record[start..used];
        let mut ids = [0u16; PROOF_ATTRIBUTES];
        let mut previous_type = 0;
        loop {
            assert!(remaining.len() >= REFERENCE_ALIGNMENT);
            if le_u32(remaining, 0) == ATTRIBUTE_END {
                break;
            }
            let attribute = NtfsAttribute::parse(remaining);
            assert!(attribute.is_some());
            let Some(attribute) = attribute else {
                return ChainChecks {
                    accepted,
                    reference,
                    count,
                    nonresident,
                };
            };
            assert!(attribute.len() >= REFERENCE_RESIDENT_HEADER_BYTES);
            assert!(attribute.len().is_multiple_of(REFERENCE_ALIGNMENT));
            assert!(attribute.header.type_id >= previous_type);
            assert!(
                (REFERENCE_ATTRIBUTE_TYPE_STEP..=REFERENCE_MAX_ATTRIBUTE_TYPE)
                    .contains(&attribute.header.type_id)
            );
            assert!(
                attribute
                    .header
                    .type_id
                    .is_multiple_of(REFERENCE_ATTRIBUTE_TYPE_STEP)
            );
            previous_type = attribute.header.type_id;
            assert!(count < ids.len());
            assert!(!ids[..count].contains(&attribute.header.id));
            ids[count] = attribute.header.id;
            count += 1;
            nonresident |= attribute.header.is_non_resident == 1;
            if matches!(
                attribute.header.type_id,
                REFERENCE_STANDARD_INFORMATION | REFERENCE_ATTRIBUTE_LIST | REFERENCE_FILE_NAME
            ) {
                assert_eq!(attribute.header.name_length, 0);
                assert_eq!(attribute.header.flags, 0);
            }
            let rest = remaining.get(attribute.len()..);
            assert!(rest.is_some());
            let Some(rest) = rest else {
                return ChainChecks {
                    accepted,
                    reference,
                    count,
                    nonresident,
                };
            };
            assert!(rest.len() < remaining.len());
            remaining = rest;
        }
    }
    ChainChecks {
        accepted,
        reference,
        count,
        nonresident,
    }
}

#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn chains_at_offset_forty_eight_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_48>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.accepted && checked.count > 1);
    kani::cover!(checked.accepted && checked.nonresident);
}

#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn chains_at_offset_fifty_six_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_56>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn chains_at_offset_sixty_four_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_64>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn chains_at_offset_seventy_two_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_72>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn chains_at_offset_eighty_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_80>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn chains_at_offset_eighty_eight_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_88>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn chains_at_offset_ninety_six_preserve_admission_and_invariants() {
    let checked = check_chain_at::<ATTRIBUTE_OFFSET_96>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn terminal_only_offsets_preserve_admission() {
    let bytes: [u8; PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let start = le_u16(&bytes, ATTRIBUTES_OFFSET_FIELD);
    kani::assume(TERMINATOR_STARTS.contains(&start));
    let record = &bytes[..length];
    let accepted = attributes_complete(record);
    assert_eq!(accepted, admission_reference::attributes_complete(record));
    if accepted {
        assert_eq!(le_u32(record, usize::from(start)), ATTRIBUTE_END);
    }
    kani::cover!(accepted);
    kani::cover!(!accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn remaining_attribute_offsets_reject_the_chain() {
    let bytes: [u8; PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let start = le_u16(&bytes, ATTRIBUTES_OFFSET_FIELD);
    kani::assume(
        !matches!(
            start,
            ATTRIBUTE_OFFSET_48
                | ATTRIBUTE_OFFSET_56
                | ATTRIBUTE_OFFSET_64
                | ATTRIBUTE_OFFSET_72
                | ATTRIBUTE_OFFSET_80
                | ATTRIBUTE_OFFSET_88
                | ATTRIBUTE_OFFSET_96
        ) && !TERMINATOR_STARTS.contains(&start),
    );
    let record = &bytes[..length];
    assert!(!attributes_complete(record));
    assert!(!admission_reference::attributes_complete(record));
    kani::cover!(length == 0);
    kani::cover!(length == PROOF_BYTES);
}
