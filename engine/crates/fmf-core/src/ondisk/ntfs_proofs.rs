use super::{
    MAX_CLUSTER_BYTES, MAX_FILE_RECORD_BYTES, NONRESIDENT_HEADER_BYTES, NtfsAttribute,
    RESIDENT_HEADER_BYTES, SECTOR_BYTES, decode_boot_sector,
};

const MAX_SECTOR_BYTES: u64 = 4096;
const ATTRIBUTE_PROOF_BYTES: usize = NONRESIDENT_HEADER_BYTES * 2;
const ATTRIBUTE_NAME_UNIT_BYTES: usize = size_of::<u16>();

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
