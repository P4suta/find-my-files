use super::{
    MAX_FILE_RECORD_BYTES, NONRESIDENT_HEADER_BYTES, NtfsAttribute, RESIDENT_HEADER_BYTES,
    SECTOR_BYTES, decode_boot_sector,
};

const MAX_SECTOR_BYTES: u64 = 4096;
const MAX_CLUSTER_BYTES: u64 = 2 << 20;
const ATTRIBUTE_PROOF_BYTES: usize = NONRESIDENT_HEADER_BYTES * 2;
const _: () = assert!(ATTRIBUTE_PROOF_BYTES <= u8::MAX as usize);
const ATTRIBUTE_NAME_UNIT_BYTES: usize = size_of::<u16>();
const REFERENCE_USA_OFFSET_FIELD: usize = 4;
const REFERENCE_USA_LENGTH_FIELD: usize = 6;
const REFERENCE_SEQUENCE_SHIFT: u32 = 48;
const REFERENCE_ATTRIBUTE_ALIGNMENT: usize = 8;

#[kani::proof]
#[kani::unwind(43)]
fn shared_file_geometry_preserves_header_admission() {
    let bytes: [u8; 1024] = kani::any();
    let length: usize = kani::any();
    let sector_size: usize = kani::any();
    kani::assume(length <= bytes.len());
    let data = &bytes[..length];
    let actual = super::NtfsFile::is_valid(data, sector_size);
    let expected = super::decode_file_header(data)
        .is_some_and(|header| file_header_admission_reference(data, header, sector_size));
    assert_eq!(actual, expected);
    kani::cover!(!actual);
    kani::cover!(actual && length == 512);
    kani::cover!(actual && length == 1024);
}

fn file_header_admission_reference(
    data: &[u8],
    header: super::NtfsFileRecordHeader,
    sector_size: usize,
) -> bool {
    let Some(update_sequence_offset) =
        super::le_u16(data, REFERENCE_USA_OFFSET_FIELD).map(usize::from)
    else {
        return false;
    };
    let Some(update_sequence_length) =
        super::le_u16(data, REFERENCE_USA_LENGTH_FIELD).map(usize::from)
    else {
        return false;
    };
    let Some(update_sequence_bytes) = update_sequence_length.checked_mul(size_of::<u16>()) else {
        return false;
    };
    let Some(update_sequence_end) = update_sequence_offset.checked_add(update_sequence_bytes)
    else {
        return false;
    };
    let Ok(used) = usize::try_from(header.used_size) else {
        return false;
    };
    let attributes_offset = usize::from(header.attributes_offset);
    let in_use = header.flags & super::FILE_RECORD_IN_USE != 0;
    update_sequence_length > 1
        && update_sequence_offset >= super::FILE_RECORD_HEADER_BYTES
        && update_sequence_offset.is_multiple_of(size_of::<u16>())
        && update_sequence_end <= data.len()
        && update_sequence_end <= attributes_offset
        && (SECTOR_BYTES..=super::MAX_SECTOR_BYTES).contains(&sector_size)
        && sector_size.is_power_of_two()
        && data.len().is_multiple_of(sector_size)
        && update_sequence_length == data.len() / sector_size + 1
        && usize::try_from(header.allocated_size).ok() == Some(data.len())
        && (!in_use || header.sequence_value != 0)
        && (header.base_reference == 0 || header.base_reference >> REFERENCE_SEQUENCE_SHIFT != 0)
        && used <= data.len()
        && attributes_offset >= super::FILE_RECORD_HEADER_BYTES
        && attributes_offset.is_multiple_of(REFERENCE_ATTRIBUTE_ALIGNMENT)
        && attributes_offset < used
}

#[kani::proof]
#[kani::unwind(513)]
fn accepted_boot_geometry_contains_a_complete_record_without_overflow() {
    let bytes: [u8; SECTOR_BYTES] = kani::any();
    let geometry = decode_boot_sector(&bytes);
    if let Some(geometry) = geometry {
        assert!((SECTOR_BYTES as u64..=MAX_SECTOR_BYTES).contains(&geometry.sector_size));
        assert!(geometry.sector_size.is_power_of_two());
        assert!(geometry.cluster_size >= geometry.sector_size);
        assert!(geometry.cluster_size <= MAX_CLUSTER_BYTES);
        assert!(geometry.cluster_size.is_power_of_two());
        assert!(geometry.cluster_size.is_multiple_of(geometry.sector_size));
        assert!(geometry.volume_size > 0);
        assert!(geometry.volume_size.is_multiple_of(geometry.sector_size));
        assert!(geometry.file_record_size >= geometry.sector_size);
        assert!(geometry.file_record_size <= MAX_FILE_RECORD_BYTES);
        assert!(geometry.file_record_size.is_power_of_two());
        assert!(
            geometry
                .file_record_size
                .is_multiple_of(geometry.sector_size)
        );
        assert!(geometry.mft_position.is_multiple_of(geometry.cluster_size));
        assert!(geometry.mft_position < geometry.volume_size);
        assert!(geometry.file_record_size <= geometry.volume_size - geometry.mft_position);
    }
    kani::cover!(geometry.is_some());
    kani::cover!(geometry.is_none());
}

#[kani::proof]
#[kani::unwind(9)]
fn reference_bytes_preserve_record_and_sequence_fields() {
    let number: u64 = kani::any();
    let sequence_value: u16 = kani::any();
    let file = super::NtfsFile {
        number,
        data: &[],
        header: super::NtfsFileRecordHeader {
            sequence_value,
            attributes_offset: 0,
            flags: 0,
            used_size: 0,
            allocated_size: 0,
            base_reference: 0,
        },
    };
    let bytes = file.reference_number().to_le_bytes();
    assert_eq!(&bytes[..6], &number.to_le_bytes()[..6]);
    assert_eq!(&bytes[6..], &sequence_value.to_le_bytes());
    kani::cover!(number == u64::MAX && sequence_value == 0);
    kani::cover!(number == 0 && sequence_value == u16::MAX);
}

#[kani::proof]
#[kani::unwind(129)]
#[kani::solver(cadical)]
fn accepted_name_values_have_bounded_units_namespaces_and_parent_references() {
    let bytes: [u8; ATTRIBUTE_PROOF_BYTES] = kani::any();
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let name = super::NtfsFileName::parse(&bytes[..length]);
    if let Some(name) = &name {
        assert!(name.header.name_length > 0);
        assert_eq!(name.utf16le.len(), usize::from(name.header.name_length) * 2);
        assert!(name.header.namespace <= super::NtfsFileNamespace::Win32AndDos as u8);
        assert!(name.header.parent_directory_reference >> super::SEQUENCE_SHIFT > 0);
        let position: u8 = kani::any();
        let offset = usize::from(position) * ATTRIBUTE_NAME_UNIT_BYTES;
        if let Some(pair) = name.utf16le.get(offset..offset + ATTRIBUTE_NAME_UNIT_BYTES) {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            assert!(unit != 0 && unit != u16::from(b'/') && unit != u16::from(b'\\'));
        }
    }
    kani::cover!(name.is_some());
    kani::cover!(name.is_none());
}

#[kani::proof]
#[kani::unwind(9)]
fn attribute_traversal_consumes_bounded_positive_lengths() {
    let bytes: [u8; ATTRIBUTE_PROOF_BYTES] = kani::any();
    let file = super::NtfsFile {
        number: 0,
        data: &bytes,
        header: super::NtfsFileRecordHeader {
            sequence_value: 0,
            attributes_offset: kani::any(),
            flags: 0,
            used_size: kani::any(),
            allocated_size: 0,
            base_reference: 0,
        },
    };
    let mut consumed = 0;
    let mut visited = 0;
    file.attributes(|attribute| {
        assert!(attribute.len() >= RESIDENT_HEADER_BYTES);
        consumed += attribute.len();
        visited += 1;
        assert!(consumed <= bytes.len());
    });
    assert!(visited <= bytes.len() / RESIDENT_HEADER_BYTES);
    kani::cover!(visited == 0);
    kani::cover!(visited > 1);
}

#[kani::proof]
#[kani::unwind(2)]
fn every_short_boot_sector_is_rejected_before_field_access() {
    let bytes: [u8; SECTOR_BYTES] = kani::any();
    let len: usize = kani::any();
    kani::assume(len < bytes.len());
    assert!(decode_boot_sector(&bytes[..len]).is_none());
    kani::cover!(len == 0);
    kani::cover!(len == SECTOR_BYTES - 1);
}

#[kani::proof]
#[kani::unwind(129)]
fn accepted_attribute_ranges_remain_inside_the_declared_buffer() {
    let bytes: [u8; ATTRIBUTE_PROOF_BYTES] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= bytes.len());
    let attribute = NtfsAttribute::parse(&bytes[..len]);
    if let Some(attribute) = &attribute {
        assert!(attribute.len() <= len);
        assert_eq!(attribute.len(), attribute.header.length as usize);
        assert!(matches!(attribute.header.is_non_resident, 0 | 1));
        let header_bytes = if attribute.header.is_non_resident == 0 {
            RESIDENT_HEADER_BYTES
        } else {
            NONRESIDENT_HEADER_BYTES
        };
        assert!(attribute.len() >= header_bytes);
        if let Some(range) = attribute.resident_value_range() {
            assert_eq!(attribute.header.is_non_resident, 0);
            assert!(range.start >= RESIDENT_HEADER_BYTES);
            assert!(range.start <= range.end && range.end <= attribute.len());
        }
        if attribute.header.name_length > 0 {
            let start = usize::from(attribute.header.name_offset);
            let name_bytes = usize::from(attribute.header.name_length) * ATTRIBUTE_NAME_UNIT_BYTES;
            assert!(start >= header_bytes);
            assert!(start + name_bytes <= attribute.len());
        }
    }
    kani::cover!(attribute.is_none());
    kani::cover!(
        attribute
            .as_ref()
            .is_some_and(|a| a.header.is_non_resident == 0)
    );
    kani::cover!(
        attribute
            .as_ref()
            .is_some_and(|a| a.header.is_non_resident == 1)
    );
}
