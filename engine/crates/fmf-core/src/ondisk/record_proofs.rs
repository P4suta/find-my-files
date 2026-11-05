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
const REFERENCE_NONRESIDENT_HEADER_BYTES: usize = 64;
const REFERENCE_RESIDENCY_FIELD: usize = 8;
const REFERENCE_ALIGNMENT: usize = 8;
const REFERENCE_ATTRIBUTE_TYPE_STEP: u32 = 0x10;
const REFERENCE_MAX_ATTRIBUTE_TYPE: u32 = 0x100;
const REFERENCE_STANDARD_INFORMATION: u32 = 0x10;
const REFERENCE_ATTRIBUTE_LIST: u32 = 0x20;
const REFERENCE_FILE_NAME: u32 = 0x30;
const TERMINATOR_OFFSET_104: u16 = 104;
const TERMINATOR_OFFSET_112: u16 = 112;
const TERMINATOR_OFFSET_120: u16 = 120;
const ATTRIBUTE_OFFSET_48: u16 = 48;
const ATTRIBUTE_OFFSET_56: u16 = 56;
const ATTRIBUTE_OFFSET_64: u16 = 64;
const ATTRIBUTE_OFFSET_72: u16 = 72;
const ATTRIBUTE_OFFSET_80: u16 = 80;
const ATTRIBUTE_OFFSET_88: u16 = 88;
const ATTRIBUTE_OFFSET_96: u16 = 96;

const _: () = {
    assert!(
        PROOF_BYTES - (ATTRIBUTE_OFFSET_48 as usize)
            < REFERENCE_NONRESIDENT_HEADER_BYTES + REFERENCE_RESIDENT_HEADER_BYTES
    );
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_48 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 2 <= 5
    );
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_64 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 2 <= 4
    );
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_88 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 2 <= 3
    );
    assert!(
        PROOF_BYTES - (TERMINATOR_OFFSET_104 as usize)
            < REFERENCE_RESIDENT_HEADER_BYTES + REFERENCE_ALIGNMENT
    );
    assert!(PROOF_BYTES == TERMINATOR_OFFSET_120 as usize + REFERENCE_ALIGNMENT);
};

struct ChainChecks {
    accepted: bool,
    count: usize,
    nonresident: bool,
}

struct AdmissionChecks {
    accepted: bool,
    reference: bool,
}

fn record_bytes_at<const START: u16>() -> ([u8; PROOF_BYTES], usize) {
    let mut bytes: [u8; PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length_bytes = u32::from(length).to_le_bytes();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let used: u8 = kani::any();
    let [offset_low, offset_high] = START.to_le_bytes();
    bytes[ATTRIBUTES_OFFSET_FIELD] = offset_low;
    bytes[ATTRIBUTES_OFFSET_FIELD + 1] = offset_high;
    bytes[USED_BYTES_FIELD..USED_BYTES_FIELD + size_of::<u32>()]
        .copy_from_slice(&u32::from(used).to_le_bytes());
    bytes[ALLOCATED_BYTES_FIELD..ALLOCATED_BYTES_FIELD + size_of::<u32>()]
        .copy_from_slice(&length_bytes);
    (bytes, length)
}

fn record_bytes_with_marker_at<const START: u16, const MARKER: u8>() -> ([u8; PROOF_BYTES], usize) {
    const { assert!(MARKER <= 1) };
    let (mut bytes, length) = record_bytes_at::<START>();
    bytes[usize::from(START) + REFERENCE_RESIDENCY_FIELD] = MARKER;
    (bytes, length)
}

fn check_admission_at<const START: u16, const MARKER: u8>() -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_marker_at::<START, MARKER>();
    let record = &bytes[..length];
    AdmissionChecks {
        accepted: attributes_complete(record),
        reference: admission_reference::attributes_complete(record),
    }
}

fn check_chain_invariants_at<const START: u16, const MARKER: u8>() -> ChainChecks {
    let (bytes, length) = record_bytes_with_marker_at::<START, MARKER>();
    let record = &bytes[..length];
    let accepted = attributes_complete(record);
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
        count,
        nonresident,
    }
}

#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_forty_eight_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_48, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_forty_eight_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_48, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_forty_eight_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_48, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.accepted && checked.count > 1);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_forty_eight_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_48, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.accepted && checked.nonresident);
}

#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_fifty_six_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_56, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_fifty_six_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_56, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_fifty_six_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_56, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_fifty_six_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_56, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_sixty_four_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_64, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_sixty_four_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_64, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_sixty_four_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_64, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_64) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_sixty_four_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_64, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_seventy_two_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_72, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_seventy_two_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_72, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_seventy_two_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_72, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_72) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_seventy_two_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_72, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_eighty_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_80, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_80, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_eighty_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_80, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_80) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_80, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_eighty_eight_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_88, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_eight_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_88, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_eighty_eight_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_88, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_88) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_eight_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_88, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_ninety_six_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_96, 0>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_ninety_six_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_96, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_chains_at_offset_ninety_six_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_96, 0>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_96) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_ninety_six_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_96, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

struct InvalidMarkerChecks {
    accepted: bool,
    marker: u8,
    first_type: u32,
}

fn check_invalid_marker_at<const START: u16>() -> InvalidMarkerChecks {
    let (bytes, length) = record_bytes_at::<START>();
    let marker = bytes[usize::from(START) + REFERENCE_RESIDENCY_FIELD];
    kani::assume(marker > 1);
    let record = &bytes[..length];
    let accepted = attributes_complete(record);
    assert_eq!(accepted, admission_reference::attributes_complete(record));
    if accepted {
        assert!(record.len() >= REFERENCE_FILE_HEADER_BYTES);
        let used = le_u32(record, USED_BYTES_FIELD) as usize;
        let allocated = le_u32(record, ALLOCATED_BYTES_FIELD) as usize;
        let start = usize::from(le_u16(record, ATTRIBUTES_OFFSET_FIELD));
        assert_eq!(allocated, record.len());
        assert_eq!(start, usize::from(START));
        assert!(start >= REFERENCE_FILE_HEADER_BYTES && start < used && used <= allocated);
        assert!(start.is_multiple_of(REFERENCE_ALIGNMENT));
        assert!(used - start >= REFERENCE_ALIGNMENT);
    }
    InvalidMarkerChecks {
        accepted,
        marker,
        first_type: le_u32(&bytes, usize::from(START)),
    }
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_forty_eight_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_48>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_fifty_six_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_56>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_sixty_four_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_64>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_seventy_two_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_72>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_eighty_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_80>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_eighty_eight_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_88>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn invalid_markers_at_offset_ninety_six_allow_only_an_immediate_terminator() {
    let checked = check_invalid_marker_at::<ATTRIBUTE_OFFSET_96>();
    assert!(!checked.accepted || checked.first_type == ATTRIBUTE_END);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.marker == 2 && checked.accepted);
    kani::cover!(checked.marker == u8::MAX && !checked.accepted);
}

struct TerminalChecks {
    accepted: bool,
    reference: bool,
    type_id: Option<u32>,
}

fn check_terminal_at<const START: u16>() -> TerminalChecks {
    let (bytes, length) = record_bytes_at::<START>();
    let record = &bytes[..length];
    let accepted = attributes_complete(record);
    TerminalChecks {
        accepted,
        reference: admission_reference::attributes_complete(record),
        type_id: accepted.then(|| le_u32(record, usize::from(START))),
    }
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn terminal_at_offset_one_hundred_four_preserves_admission() {
    let checked = check_terminal_at::<TERMINATOR_OFFSET_104>();
    assert_eq!(checked.accepted, checked.reference);
    if checked.accepted {
        assert_eq!(checked.type_id, Some(ATTRIBUTE_END));
    }
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn terminal_at_offset_one_hundred_twelve_preserves_admission() {
    let checked = check_terminal_at::<TERMINATOR_OFFSET_112>();
    assert_eq!(checked.accepted, checked.reference);
    if checked.accepted {
        assert_eq!(checked.type_id, Some(ATTRIBUTE_END));
    }
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn terminal_at_offset_one_hundred_twenty_preserves_admission() {
    let checked = check_terminal_at::<TERMINATOR_OFFSET_120>();
    assert_eq!(checked.accepted, checked.reference);
    if checked.accepted {
        assert_eq!(checked.type_id, Some(ATTRIBUTE_END));
    }
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(cadical)]
fn inconsistent_record_lengths_reject_before_attribute_traversal() {
    let bytes: [u8; PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let allocated = le_u32(&bytes, ALLOCATED_BYTES_FIELD) as usize;
    let used = le_u32(&bytes, USED_BYTES_FIELD) as usize;
    kani::assume(allocated != length || used > length);
    let record = &bytes[..length];
    assert!(!attributes_complete(record));
    assert!(!admission_reference::attributes_complete(record));
    kani::cover!(length >= REFERENCE_FILE_HEADER_BYTES && allocated != length);
    kani::cover!(length >= REFERENCE_FILE_HEADER_BYTES && allocated == length && used > length);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn remaining_attribute_offsets_reject_the_chain() {
    let bytes: [u8; PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let start = le_u16(&bytes, ATTRIBUTES_OFFSET_FIELD);
    kani::assume(!matches!(
        start,
        ATTRIBUTE_OFFSET_48
            | ATTRIBUTE_OFFSET_56
            | ATTRIBUTE_OFFSET_64
            | ATTRIBUTE_OFFSET_72
            | ATTRIBUTE_OFFSET_80
            | ATTRIBUTE_OFFSET_88
            | ATTRIBUTE_OFFSET_96
            | TERMINATOR_OFFSET_104
            | TERMINATOR_OFFSET_112
            | TERMINATOR_OFFSET_120
    ));
    let record = &bytes[..length];
    assert!(!attributes_complete(record));
    assert!(!admission_reference::attributes_complete(record));
    kani::cover!(length == 0);
    kani::cover!(length == PROOF_BYTES);
}
