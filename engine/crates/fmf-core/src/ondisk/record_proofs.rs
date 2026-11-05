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
const REFERENCE_ATTRIBUTE_LENGTH_FIELD: usize = 4;
const REFERENCE_RESIDENCY_FIELD: usize = 8;
const REFERENCE_ATTRIBUTE_ID_FIELD: usize = 14;
const REFERENCE_HIGH_ID_START: u16 = u128::BITS as u16;
const REFERENCE_ALIGNMENT: usize = 8;
const REFERENCE_ATTRIBUTE_TYPE_STEP: u32 = 0x10;
const REFERENCE_MAX_ATTRIBUTE_TYPE: u32 = 0x100;
const REFERENCE_STANDARD_INFORMATION: u32 = 0x10;
const REFERENCE_ATTRIBUTE_LIST: u32 = 0x20;
const REFERENCE_FILE_NAME: u32 = 0x30;
const FINAL_ATTRIBUTE_CAPACITY: usize = 32;
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
        (PROOF_BYTES - ATTRIBUTE_OFFSET_48 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 1 <= 4
    );
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_64 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 1 <= 3
    );
    assert!(
        (PROOF_BYTES - ATTRIBUTE_OFFSET_88 as usize) / REFERENCE_RESIDENT_HEADER_BYTES + 1 <= 2
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrefixIds {
    BitsetPair,
    FirstHigh,
    SecondHigh,
    HighPair,
}

fn prefix_ids(first: u16, second: u16) -> PrefixIds {
    match (
        first >= REFERENCE_HIGH_ID_START,
        second >= REFERENCE_HIGH_ID_START,
    ) {
        (false, false) => PrefixIds::BitsetPair,
        (true, false) => PrefixIds::FirstHigh,
        (false, true) => PrefixIds::SecondHigh,
        (true, true) => PrefixIds::HighPair,
    }
}

#[kani::proof]
#[kani::unwind(1)]
fn prefix_id_partitions_preserve_every_id_pair() {
    let first: u16 = kani::any();
    let second: u16 = kani::any();
    let class = prefix_ids(first, second);
    let first_high = first >= REFERENCE_HIGH_ID_START;
    let second_high = second >= REFERENCE_HIGH_ID_START;
    assert_eq!(class == PrefixIds::BitsetPair, !first_high && !second_high);
    assert_eq!(class == PrefixIds::FirstHigh, first_high && !second_high);
    assert_eq!(class == PrefixIds::SecondHigh, !first_high && second_high);
    assert_eq!(class == PrefixIds::HighPair, first_high && second_high);
    kani::cover!(class == PrefixIds::BitsetPair);
    kani::cover!(class == PrefixIds::FirstHigh && first == u16::MAX);
    kani::cover!(class == PrefixIds::SecondHigh && second == REFERENCE_HIGH_ID_START);
    kani::cover!(class == PrefixIds::HighPair && first != second);
}

fn prefix_ids_at<const START: u16, const FIRST: u32>(bytes: &[u8; PROOF_BYTES]) -> PrefixIds {
    let offset = usize::from(START) + REFERENCE_ATTRIBUTE_ID_FIELD;
    prefix_ids(
        le_u16(bytes, offset),
        le_u16(bytes, offset + FIRST as usize),
    )
}

fn check_prefix_ids<
    const START: u16,
    const FIRST: u32,
    const FIRST_HIGH: bool,
    const SECOND_HIGH: bool,
>(
    bytes: &[u8; PROOF_BYTES],
) {
    let offset = usize::from(START) + REFERENCE_ATTRIBUTE_ID_FIELD;
    assert_eq!(le_u16(bytes, offset) >= REFERENCE_HIGH_ID_START, FIRST_HIGH);
    assert_eq!(
        le_u16(bytes, offset + FIRST as usize) >= REFERENCE_HIGH_ID_START,
        SECOND_HIGH
    );
}

fn check_admission_for_prefix_ids<
    const START: u16,
    const FIRST: u32,
    const FIRST_HIGH: bool,
    const SECOND_HIGH: bool,
>(
    bytes: &[u8; PROOF_BYTES],
    length: usize,
) -> AdmissionChecks {
    check_prefix_ids::<START, FIRST, FIRST_HIGH, SECOND_HIGH>(bytes);
    let record = &bytes[..length];
    AdmissionChecks {
        accepted: attributes_complete(record),
        reference: admission_reference::attributes_complete(record),
    }
}

fn check_admission_by_prefix_ids<const START: u16, const FIRST: u32>(
    bytes: &[u8; PROOF_BYTES],
    length: usize,
) -> AdmissionChecks {
    match prefix_ids_at::<START, FIRST>(bytes) {
        PrefixIds::BitsetPair => {
            check_admission_for_prefix_ids::<START, FIRST, false, false>(bytes, length)
        }
        PrefixIds::FirstHigh => {
            check_admission_for_prefix_ids::<START, FIRST, true, false>(bytes, length)
        }
        PrefixIds::SecondHigh => {
            check_admission_for_prefix_ids::<START, FIRST, false, true>(bytes, length)
        }
        PrefixIds::HighPair => {
            check_admission_for_prefix_ids::<START, FIRST, true, true>(bytes, length)
        }
    }
}

fn check_invariants_for_prefix_ids<
    const START: u16,
    const FIRST: u32,
    const FIRST_HIGH: bool,
    const SECOND_HIGH: bool,
>(
    bytes: &[u8; PROOF_BYTES],
    length: usize,
) -> ChainChecks {
    check_prefix_ids::<START, FIRST, FIRST_HIGH, SECOND_HIGH>(bytes);
    check_chain_invariants(&bytes[..length])
}

fn check_invariants_by_prefix_ids<const START: u16, const FIRST: u32>(
    bytes: &[u8; PROOF_BYTES],
    length: usize,
) -> ChainChecks {
    match prefix_ids_at::<START, FIRST>(bytes) {
        PrefixIds::BitsetPair => {
            check_invariants_for_prefix_ids::<START, FIRST, false, false>(bytes, length)
        }
        PrefixIds::FirstHigh => {
            check_invariants_for_prefix_ids::<START, FIRST, true, false>(bytes, length)
        }
        PrefixIds::SecondHigh => {
            check_invariants_for_prefix_ids::<START, FIRST, false, true>(bytes, length)
        }
        PrefixIds::HighPair => {
            check_invariants_for_prefix_ids::<START, FIRST, true, true>(bytes, length)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaterResidency {
    OtherSecondMarker,
    OtherThirdMarker,
    ResidentMarkers,
}

fn later_residency(second: u8, third: u8) -> LaterResidency {
    match (second, third) {
        (0, 0) => LaterResidency::ResidentMarkers,
        (0, _) => LaterResidency::OtherThirdMarker,
        _ => LaterResidency::OtherSecondMarker,
    }
}

#[kani::proof]
#[kani::unwind(1)]
fn later_residency_partitions_preserve_every_marker_pair() {
    let second: u8 = kani::any();
    let third: u8 = kani::any();
    let class = later_residency(second, third);
    assert_eq!(class == LaterResidency::OtherSecondMarker, second != 0);
    assert_eq!(
        class == LaterResidency::OtherThirdMarker,
        second == 0 && third != 0
    );
    assert_eq!(
        class == LaterResidency::ResidentMarkers,
        second == 0 && third == 0
    );
    kani::cover!(class == LaterResidency::OtherSecondMarker && second == u8::MAX);
    kani::cover!(class == LaterResidency::OtherThirdMarker && third == 1);
    kani::cover!(class == LaterResidency::ResidentMarkers);
}

#[kani::proof]
#[kani::unwind(1)]
fn resident_length_partitions_preserve_every_field_value() {
    let length: u32 = kani::any();
    let available: u8 = kani::any();
    kani::assume(available <= 80 && available.is_multiple_of(8));
    let available = usize::from(available);
    let explicit_case = matches!(length, 24 | 32 | 40 | 48 | 56 | 64 | 72)
        && available
            .checked_sub(8)
            .is_some_and(|maximum| length as usize <= maximum);
    assert_ne!(resident_length_is_other(length, available), explicit_case);
    kani::cover!(length == 72 && available == 80 && explicit_case);
    kani::cover!(length == u32::MAX && !explicit_case);
    kani::cover!(available == 0 && !explicit_case);
}

#[kani::proof]
#[kani::unwind(1)]
fn final_resident_length_partitions_preserve_every_field_value() {
    let length: u32 = kani::any();
    let available: u8 = kani::any();
    kani::assume(usize::from(available) <= FINAL_ATTRIBUTE_CAPACITY && available.is_multiple_of(8));
    let explicit_case = matches!(length, 24 | 32) && length <= u32::from(available);
    assert_ne!(
        resident_length_is_invalid(length, usize::from(available)),
        explicit_case
    );
    kani::cover!(length == 24 && available == 32 && explicit_case);
    kani::cover!(length == 32 && available == 32 && explicit_case);
    kani::cover!(length == u32::MAX && !explicit_case);
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

fn resident_length_is_other(length: u32, available: usize) -> bool {
    let Some(maximum) = available.checked_sub(REFERENCE_ALIGNMENT) else {
        return true;
    };
    resident_length_is_invalid(length, maximum)
}

fn resident_length_is_invalid(length: u32, maximum: usize) -> bool {
    let length = length as usize;
    length < REFERENCE_RESIDENT_HEADER_BYTES
        || !length.is_multiple_of(REFERENCE_ALIGNMENT)
        || length > maximum
}

fn check_admission_at<const START: u16, const MARKER: u8>() -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_marker_at::<START, MARKER>();
    let record = &bytes[..length];
    AdmissionChecks {
        accepted: attributes_complete(record),
        reference: admission_reference::attributes_complete(record),
    }
}

fn record_bytes_with_first_length<const START: u16, const LENGTH: u32>()
-> ([u8; PROOF_BYTES], usize) {
    const {
        assert!(LENGTH as usize >= REFERENCE_RESIDENT_HEADER_BYTES);
        assert!((LENGTH as usize).is_multiple_of(REFERENCE_ALIGNMENT));
        assert!(START as usize + LENGTH as usize + REFERENCE_ALIGNMENT <= PROOF_BYTES);
    }
    let (mut bytes, length) = record_bytes_with_marker_at::<START, 0>();
    let field = usize::from(START) + REFERENCE_ATTRIBUTE_LENGTH_FIELD;
    bytes[field..field + size_of::<u32>()].copy_from_slice(&LENGTH.to_le_bytes());
    (bytes, length)
}

fn check_resident_admission_with_length<const START: u16, const LENGTH: u32>() -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_first_length::<START, LENGTH>();
    let record = &bytes[..length];
    AdmissionChecks {
        accepted: attributes_complete(record),
        reference: admission_reference::attributes_complete(record),
    }
}

fn record_bytes_with_other_first_lengths<const START: u16>() -> ([u8; PROOF_BYTES], usize) {
    const { assert!(START as usize + REFERENCE_ALIGNMENT <= PROOF_BYTES) };
    let (bytes, length) = record_bytes_with_marker_at::<START, 0>();
    let first_length = le_u32(
        &bytes,
        usize::from(START) + REFERENCE_ATTRIBUTE_LENGTH_FIELD,
    );
    kani::assume(resident_length_is_other(
        first_length,
        PROOF_BYTES - usize::from(START),
    ));
    (bytes, length)
}

fn check_resident_admission_with_other_lengths<const START: u16>() -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_other_first_lengths::<START>();
    let record = &bytes[..length];
    AdmissionChecks {
        accepted: attributes_complete(record),
        reference: admission_reference::attributes_complete(record),
    }
}

fn record_bytes_with_second_length<const START: u16, const FIRST: u32>(
    second: Option<u32>,
) -> ([u8; PROOF_BYTES], usize) {
    const {
        assert!(START as usize + FIRST as usize + REFERENCE_ALIGNMENT <= PROOF_BYTES);
    }
    let (mut bytes, length) = record_bytes_with_first_length::<START, FIRST>();
    let offset = usize::from(START) + FIRST as usize;
    let field = offset + REFERENCE_ATTRIBUTE_LENGTH_FIELD;
    if let Some(second) = second {
        assert!(!resident_length_is_other(second, PROOF_BYTES - offset));
        bytes[field..field + size_of::<u32>()].copy_from_slice(&second.to_le_bytes());
    } else {
        let second = le_u32(&bytes, field);
        kani::assume(resident_length_is_other(second, PROOF_BYTES - offset));
    }
    (bytes, length)
}

fn check_resident_admission_with_second_length<const START: u16, const FIRST: u32>(
    second: Option<u32>,
) -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_second_length::<START, FIRST>(second);
    let record = &bytes[..length];
    AdmissionChecks {
        accepted: attributes_complete(record),
        reference: admission_reference::attributes_complete(record),
    }
}

fn record_bytes_with_later_residency<const START: u16, const FIRST: u32, const SECOND: u32>(
    class: LaterResidency,
) -> ([u8; PROOF_BYTES], usize) {
    const {
        assert!(
            START as usize + FIRST as usize + SECOND as usize + REFERENCE_RESIDENCY_FIELD
                < PROOF_BYTES
        );
        assert!(
            PROOF_BYTES - START as usize - FIRST as usize - SECOND as usize
                <= FINAL_ATTRIBUTE_CAPACITY
        );
    }
    let (mut bytes, length) = record_bytes_with_second_length::<START, FIRST>(Some(SECOND));
    let second = usize::from(START) + FIRST as usize + REFERENCE_RESIDENCY_FIELD;
    let third = second + SECOND as usize;
    match class {
        LaterResidency::OtherSecondMarker => {
            kani::assume(later_residency(bytes[second], bytes[third]) == class);
        }
        LaterResidency::OtherThirdMarker => {
            bytes[second] = 0;
            kani::assume(later_residency(bytes[second], bytes[third]) == class);
        }
        LaterResidency::ResidentMarkers => {
            bytes[second] = 0;
            bytes[third] = 0;
        }
    }
    (bytes, length)
}

fn record_bytes_with_third_length<const START: u16, const FIRST: u32, const SECOND: u32>(
    third: Option<u32>,
) -> ([u8; PROOF_BYTES], usize) {
    let (mut bytes, length) =
        record_bytes_with_later_residency::<START, FIRST, SECOND>(LaterResidency::ResidentMarkers);
    let offset = usize::from(START) + FIRST as usize + SECOND as usize;
    let field = offset + REFERENCE_ATTRIBUTE_LENGTH_FIELD;
    if let Some(third) = third {
        assert!(!resident_length_is_invalid(third, PROOF_BYTES - offset));
        bytes[field..field + size_of::<u32>()].copy_from_slice(&third.to_le_bytes());
    } else {
        let third = le_u32(&bytes, field);
        kani::assume(resident_length_is_invalid(third, PROOF_BYTES - offset));
    }
    (bytes, length)
}

fn check_admission_with_later_residency<const START: u16, const FIRST: u32, const SECOND: u32>(
    class: LaterResidency,
) -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_later_residency::<START, FIRST, SECOND>(class);
    check_admission_by_prefix_ids::<START, FIRST>(&bytes, length)
}

fn check_invariants_with_later_residency<const START: u16, const FIRST: u32, const SECOND: u32>(
    class: LaterResidency,
) -> ChainChecks {
    let (bytes, length) = record_bytes_with_later_residency::<START, FIRST, SECOND>(class);
    check_invariants_by_prefix_ids::<START, FIRST>(&bytes, length)
}

fn check_resident_admission_with_third_length<
    const START: u16,
    const FIRST: u32,
    const SECOND: u32,
>(
    third: Option<u32>,
) -> AdmissionChecks {
    let (bytes, length) = record_bytes_with_third_length::<START, FIRST, SECOND>(third);
    check_admission_by_prefix_ids::<START, FIRST>(&bytes, length)
}

fn check_resident_invariants_with_third_length<
    const START: u16,
    const FIRST: u32,
    const SECOND: u32,
>(
    third: Option<u32>,
) -> ChainChecks {
    let (bytes, length) = record_bytes_with_third_length::<START, FIRST, SECOND>(third);
    check_invariants_by_prefix_ids::<START, FIRST>(&bytes, length)
}

fn record_bytes_with_filling_attribute<
    const START: u16,
    const FIRST: u32,
    const SECOND: u32,
    const THIRD: u32,
>(
    full_extent: bool,
) -> ([u8; PROOF_BYTES], usize) {
    const { assert!(START as usize + FIRST as usize + SECOND as usize + THIRD as usize == PROOF_BYTES) };
    let (mut bytes, length) = record_bytes_with_third_length::<START, FIRST, SECOND>(Some(THIRD));
    if full_extent {
        let extent = (PROOF_BYTES as u32).to_le_bytes();
        bytes[USED_BYTES_FIELD..USED_BYTES_FIELD + size_of::<u32>()].copy_from_slice(&extent);
        bytes[ALLOCATED_BYTES_FIELD..ALLOCATED_BYTES_FIELD + size_of::<u32>()]
            .copy_from_slice(&extent);
        (bytes, PROOF_BYTES)
    } else {
        kani::assume(
            length != PROOF_BYTES || le_u32(&bytes, USED_BYTES_FIELD) != PROOF_BYTES as u32,
        );
        (bytes, length)
    }
}

fn check_filling_attribute_admission<
    const START: u16,
    const FIRST: u32,
    const SECOND: u32,
    const THIRD: u32,
>(
    full_extent: bool,
) -> AdmissionChecks {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<START, FIRST, SECOND, THIRD>(full_extent);
    check_admission_by_prefix_ids::<START, FIRST>(&bytes, length)
}

fn check_filling_attribute_invariants<
    const START: u16,
    const FIRST: u32,
    const SECOND: u32,
    const THIRD: u32,
>(
    full_extent: bool,
) -> ChainChecks {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<START, FIRST, SECOND, THIRD>(full_extent);
    check_invariants_by_prefix_ids::<START, FIRST>(&bytes, length)
}

fn check_resident_invariants_with_length<const START: u16, const LENGTH: u32>() -> ChainChecks {
    let (bytes, length) = record_bytes_with_first_length::<START, LENGTH>();
    check_chain_invariants(&bytes[..length])
}

fn check_resident_invariants_with_other_lengths<const START: u16>() -> ChainChecks {
    let (bytes, length) = record_bytes_with_other_first_lengths::<START>();
    check_chain_invariants(&bytes[..length])
}

fn check_resident_invariants_with_second_length<const START: u16, const FIRST: u32>(
    second: Option<u32>,
) -> ChainChecks {
    let (bytes, length) = record_bytes_with_second_length::<START, FIRST>(second);
    check_chain_invariants(&bytes[..length])
}

fn check_chain_invariants_at<const START: u16, const MARKER: u8>() -> ChainChecks {
    let (bytes, length) = record_bytes_with_marker_at::<START, MARKER>();
    check_chain_invariants(&bytes[..length])
}

fn check_chain_invariants(record: &[u8]) -> ChainChecks {
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
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_thirty_two_at_offset_forty_eight_with_other_record_extents_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_48, 24, 24, 32>(false);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_thirty_two_at_offset_forty_eight_with_other_record_extents_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_48, 24, 24, 32>(false);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_then_twenty_four_at_offset_forty_eight_with_other_record_extents_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_48, 24, 32, 24>(false);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_then_twenty_four_at_offset_forty_eight_with_other_record_extents_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_48, 24, 32, 24>(false);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_twenty_four_then_twenty_four_at_offset_forty_eight_with_other_record_extents_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_48, 32, 24, 24>(false);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_twenty_four_then_twenty_four_at_offset_forty_eight_with_other_record_extents_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_48, 32, 24, 24>(false);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_twenty_four_at_offset_fifty_six_with_other_record_extents_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(false);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_twenty_four_at_offset_fifty_six_with_other_record_extents_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(false);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_forty_eight_with_lengths_twenty_four_then_twenty_four_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 24>(
        LaterResidency::OtherSecondMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_forty_eight_with_lengths_twenty_four_then_twenty_four_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 24>(
        LaterResidency::OtherSecondMarker,
    );
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_forty_eight_with_lengths_twenty_four_then_twenty_four_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 24>(
        LaterResidency::OtherThirdMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_forty_eight_with_lengths_twenty_four_then_twenty_four_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 24>(
        LaterResidency::OtherThirdMarker,
    );
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
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_forty_eight_with_lengths_twenty_four_then_thirty_two_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 32>(
        LaterResidency::OtherSecondMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_forty_eight_with_lengths_twenty_four_then_thirty_two_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 32>(
        LaterResidency::OtherSecondMarker,
    );
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_forty_eight_with_lengths_twenty_four_then_thirty_two_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 32>(
        LaterResidency::OtherThirdMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_forty_eight_with_lengths_twenty_four_then_thirty_two_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_48, 24, 32>(
        LaterResidency::OtherThirdMarker,
    );
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_forty_eight_with_lengths_thirty_two_then_twenty_four_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_48, 32, 24>(
        LaterResidency::OtherSecondMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_forty_eight_with_lengths_thirty_two_then_twenty_four_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_48, 32, 24>(
        LaterResidency::OtherSecondMarker,
    );
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_forty_eight_with_lengths_thirty_two_then_twenty_four_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_48, 32, 24>(
        LaterResidency::OtherThirdMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_forty_eight_with_lengths_thirty_two_then_twenty_four_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_48, 32, 24>(
        LaterResidency::OtherThirdMarker,
    );
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_fifty_six_with_lengths_twenty_four_then_twenty_four_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_56, 24, 24>(
        LaterResidency::OtherSecondMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn other_second_markers_at_offset_fifty_six_with_lengths_twenty_four_then_twenty_four_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_56, 24, 24>(
        LaterResidency::OtherSecondMarker,
    );
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
fn other_third_markers_at_offset_fifty_six_with_lengths_twenty_four_then_twenty_four_preserve_admission()
 {
    let checked = check_admission_with_later_residency::<ATTRIBUTE_OFFSET_56, 24, 24>(
        LaterResidency::OtherThirdMarker,
    );
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_markers_at_offset_fifty_six_with_lengths_twenty_four_then_twenty_four_preserve_invariants()
 {
    let checked = check_invariants_with_later_residency::<ATTRIBUTE_OFFSET_56, 24, 24>(
        LaterResidency::OtherThirdMarker,
    );
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_twenty_four_at_offset_forty_eight_preserve_admission()
 {
    let checked =
        check_resident_admission_with_third_length::<ATTRIBUTE_OFFSET_48, 24, 24>(Some(24));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_thirty_two_at_offset_forty_eight_with_full_record_extent_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_48, 24, 24, 32>(true);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_with_other_third_lengths_at_offset_forty_eight_preserve_admission()
 {
    let checked = check_resident_admission_with_third_length::<ATTRIBUTE_OFFSET_48, 24, 24>(None);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_then_twenty_four_at_offset_forty_eight_with_full_record_extent_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_48, 24, 32, 24>(true);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_with_other_third_lengths_at_offset_forty_eight_preserve_admission()
 {
    let checked = check_resident_admission_with_third_length::<ATTRIBUTE_OFFSET_48, 24, 32>(None);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_forty_at_offset_forty_eight_preserve_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_48, 24>(Some(40));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_forty_eight_at_offset_forty_eight_preserve_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_48, 24>(Some(48));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_with_other_second_lengths_at_offset_forty_eight_preserves_admission()
{
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_48, 24>(None);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_twenty_four_then_twenty_four_at_offset_forty_eight_with_full_record_extent_preserve_admission()
 {
    let checked = check_filling_attribute_admission::<ATTRIBUTE_OFFSET_48, 32, 24, 24>(true);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_twenty_four_with_other_third_lengths_at_offset_forty_eight_preserve_admission()
 {
    let checked = check_resident_admission_with_third_length::<ATTRIBUTE_OFFSET_48, 32, 24>(None);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_thirty_two_at_offset_forty_eight_preserve_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_48, 32>(Some(32));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_forty_at_offset_forty_eight_preserve_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_48, 32>(Some(40));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_with_other_second_lengths_at_offset_forty_eight_preserves_admission()
{
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_48, 32>(None);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_at_offset_forty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_48, 40>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_eight_at_offset_forty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_48, 48>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_fifty_six_at_offset_forty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_48, 56>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_sixty_four_at_offset_forty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_48, 64>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_seventy_two_at_offset_forty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_48, 72>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_forty_eight_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_48>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_forty_eight_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_48, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_twenty_four_at_offset_forty_eight_preserve_invariants()
 {
    let checked =
        check_resident_invariants_with_third_length::<ATTRIBUTE_OFFSET_48, 24, 24>(Some(24));
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.accepted && checked.count > 1);
    kani::cover!(checked.accepted && checked.count == 3);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_twenty_four_then_thirty_two_at_offset_forty_eight_with_full_record_extent_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_48, 24, 24, 32>(true);
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
fn resident_lengths_twenty_four_then_twenty_four_with_other_third_lengths_at_offset_forty_eight_preserve_invariants()
 {
    let checked = check_resident_invariants_with_third_length::<ATTRIBUTE_OFFSET_48, 24, 24>(None);
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
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_then_twenty_four_at_offset_forty_eight_with_full_record_extent_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_48, 24, 32, 24>(true);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_with_other_third_lengths_at_offset_forty_eight_preserve_invariants()
 {
    let checked = check_resident_invariants_with_third_length::<ATTRIBUTE_OFFSET_48, 24, 32>(None);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_forty_at_offset_forty_eight_preserve_invariants() {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_48, 24>(Some(40));
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_forty_eight_at_offset_forty_eight_preserve_invariants() {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_48, 24>(Some(48));
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_with_other_second_lengths_at_offset_forty_eight_preserves_invariants()
 {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_48, 24>(None);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_twenty_four_then_twenty_four_at_offset_forty_eight_with_full_record_extent_preserve_invariants()
 {
    let checked = check_filling_attribute_invariants::<ATTRIBUTE_OFFSET_48, 32, 24, 24>(true);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_twenty_four_with_other_third_lengths_at_offset_forty_eight_preserve_invariants()
 {
    let checked = check_resident_invariants_with_third_length::<ATTRIBUTE_OFFSET_48, 32, 24>(None);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_thirty_two_at_offset_forty_eight_preserve_invariants() {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_48, 32>(Some(32));
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_thirty_two_then_forty_at_offset_forty_eight_preserve_invariants() {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_48, 32>(Some(40));
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_with_other_second_lengths_at_offset_forty_eight_preserves_invariants()
{
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_48, 32>(None);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_at_offset_forty_eight_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_48, 40>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_eight_at_offset_forty_eight_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_48, 48>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_fifty_six_at_offset_forty_eight_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_48, 56>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_sixty_four_at_offset_forty_eight_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_48, 64>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_seventy_two_at_offset_forty_eight_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_48, 72>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_forty_eight_preserve_invariants() {
    let checked = check_resident_invariants_with_other_lengths::<ATTRIBUTE_OFFSET_48>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_48) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_forty_eight_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_48, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
    kani::cover!(checked.accepted && checked.nonresident);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_bitset_prefix_ids_preserves_admission() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::BitsetPair);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, false>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_first_high_id_preserves_admission() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::FirstHigh);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, false>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_second_high_id_preserves_admission() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::SecondHigh);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, true>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_high_prefix_ids_preserves_admission() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::HighPair);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, true>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_resident_lengths_at_offset_fifty_six_with_bitset_prefix_ids_preserve_admission() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::BitsetPair);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, false>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_resident_lengths_at_offset_fifty_six_with_first_high_id_preserve_admission() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::FirstHigh);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, false>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_resident_lengths_at_offset_fifty_six_with_second_high_id_preserve_admission() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::SecondHigh);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, true>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn other_third_resident_lengths_at_offset_fifty_six_with_high_prefix_ids_preserve_admission() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::HighPair);
    let checked =
        check_admission_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, true>(&bytes, length);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_thirty_two_at_offset_fifty_six_preserve_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_56, 24>(Some(32));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_lengths_twenty_four_then_forty_at_offset_fifty_six_preserve_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_56, 24>(Some(40));
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_with_other_second_lengths_at_offset_fifty_six_preserves_admission() {
    let checked = check_resident_admission_with_second_length::<ATTRIBUTE_OFFSET_56, 24>(None);
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_at_offset_fifty_six_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_56, 32>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_at_offset_fifty_six_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_56, 40>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_eight_at_offset_fifty_six_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_56, 48>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_fifty_six_at_offset_fifty_six_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_56, 56>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_sixty_four_at_offset_fifty_six_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_56, 64>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_fifty_six_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_56>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_fifty_six_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_56, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_bitset_prefix_ids_preserves_invariants() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::BitsetPair);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, false>(&bytes, length);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_first_high_id_preserves_invariants() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::FirstHigh);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, false>(&bytes, length);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_second_high_id_preserves_invariants() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::SecondHigh);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, true>(&bytes, length);
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(4)]
#[kani::solver(bitwuzla)]
fn filling_third_resident_at_offset_fifty_six_with_high_prefix_ids_preserves_invariants() {
    let (bytes, length) =
        record_bytes_with_filling_attribute::<ATTRIBUTE_OFFSET_56, 24, 24, 24>(true);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::HighPair);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, true>(&bytes, length);
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
fn other_third_resident_lengths_at_offset_fifty_six_with_bitset_prefix_ids_preserve_invariants() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::BitsetPair);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, false>(&bytes, length);
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
fn other_third_resident_lengths_at_offset_fifty_six_with_first_high_id_preserve_invariants() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::FirstHigh);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, false>(&bytes, length);
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
fn other_third_resident_lengths_at_offset_fifty_six_with_second_high_id_preserve_invariants() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::SecondHigh);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, false, true>(&bytes, length);
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
fn other_third_resident_lengths_at_offset_fifty_six_with_high_prefix_ids_preserve_invariants() {
    let (bytes, length) = record_bytes_with_third_length::<ATTRIBUTE_OFFSET_56, 24, 24>(None);
    kani::assume(prefix_ids_at::<ATTRIBUTE_OFFSET_56, 24>(&bytes) == PrefixIds::HighPair);
    let checked =
        check_invariants_for_prefix_ids::<ATTRIBUTE_OFFSET_56, 24, true, true>(&bytes, length);
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
fn resident_lengths_twenty_four_then_thirty_two_at_offset_fifty_six_preserve_invariants() {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_56, 24>(Some(32));
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
fn resident_lengths_twenty_four_then_forty_at_offset_fifty_six_preserve_invariants() {
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_56, 24>(Some(40));
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
fn resident_length_twenty_four_with_other_second_lengths_at_offset_fifty_six_preserves_invariants()
{
    let checked = check_resident_invariants_with_second_length::<ATTRIBUTE_OFFSET_56, 24>(None);
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
fn resident_length_thirty_two_at_offset_fifty_six_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_56, 32>();
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
fn resident_length_forty_at_offset_fifty_six_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_56, 40>();
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
fn resident_length_forty_eight_at_offset_fifty_six_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_56, 48>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_fifty_six_at_offset_fifty_six_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_56, 56>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_sixty_four_at_offset_fifty_six_preserves_invariants() {
    let checked = check_resident_invariants_with_length::<ATTRIBUTE_OFFSET_56, 64>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_fifty_six_preserve_invariants() {
    let checked = check_resident_invariants_with_other_lengths::<ATTRIBUTE_OFFSET_56>();
    assert!(
        checked.count
            <= (PROOF_BYTES - usize::from(ATTRIBUTE_OFFSET_56) - REFERENCE_ALIGNMENT)
                / REFERENCE_RESIDENT_HEADER_BYTES
    );
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_fifty_six_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_56, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_at_offset_sixty_four_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_64, 24>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_at_offset_sixty_four_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_64, 32>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_forty_at_offset_sixty_four_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_64, 40>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_forty_eight_at_offset_sixty_four_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_64, 48>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_fifty_six_at_offset_sixty_four_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_64, 56>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_sixty_four_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_64>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_sixty_four_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_64, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
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
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_sixty_four_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_64, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_at_offset_seventy_two_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_72, 24>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_at_offset_seventy_two_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_72, 32>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_forty_at_offset_seventy_two_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_72, 40>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_forty_eight_at_offset_seventy_two_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_72, 48>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_seventy_two_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_72>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_seventy_two_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_72, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
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
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_seventy_two_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_72, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_at_offset_eighty_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_80, 24>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_at_offset_eighty_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_80, 32>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_forty_at_offset_eighty_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_80, 40>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_eighty_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_80>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_80, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(3)]
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
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_80, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_at_offset_eighty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_88, 24>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_thirty_two_at_offset_eighty_eight_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_88, 32>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_eighty_eight_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_88>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_eight_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_88, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
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
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_eighty_eight_preserve_invariants() {
    let checked = check_chain_invariants_at::<ATTRIBUTE_OFFSET_88, 1>();
    assert!(checked.count <= 1);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_length_twenty_four_at_offset_ninety_six_preserves_admission() {
    let checked = check_resident_admission_with_length::<ATTRIBUTE_OFFSET_96, 24>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn resident_other_lengths_at_offset_ninety_six_preserve_admission() {
    let checked = check_resident_admission_with_other_lengths::<ATTRIBUTE_OFFSET_96>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
#[kani::solver(cadical)]
fn nonresident_chains_at_offset_ninety_six_preserve_admission() {
    let checked = check_admission_at::<ATTRIBUTE_OFFSET_96, 1>();
    assert_eq!(checked.accepted, checked.reference);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(2)]
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
#[kani::unwind(2)]
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
