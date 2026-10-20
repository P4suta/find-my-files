use super::{
    FILE_HEADER_BYTES, MAX_SECTOR, SECTOR, UPDATE_SEQUENCE_WORD_BYTES, checked_fixup_layout,
};

#[kani::proof]
#[kani::unwind(2)]
fn accepted_layout_bounds_every_sector_and_update_sequence_word() {
    let record_bytes: usize = kani::any();
    let sector_size: usize = kani::any();
    let offset: u16 = kani::any();
    let count: u16 = kani::any();
    let attributes: u16 = kani::any();
    let layout = checked_fixup_layout(record_bytes, sector_size, offset, count, attributes);
    if let Some((uso, usl)) = layout {
        assert!((SECTOR..=MAX_SECTOR).contains(&sector_size));
        assert!(sector_size.is_power_of_two());
        assert!(record_bytes.is_multiple_of(sector_size));
        assert!(uso >= FILE_HEADER_BYTES);
        assert!(uso.is_multiple_of(UPDATE_SEQUENCE_WORD_BYTES));
        assert!(usl >= 2);
        assert_eq!(usl - 1, record_bytes / sector_size);
        assert!(uso + usl * UPDATE_SEQUENCE_WORD_BYTES <= usize::from(attributes));
        assert!(usize::from(attributes) < record_bytes);
        let sector: usize = kani::any();
        kani::assume(sector > 0 && sector < usl);
        let end = sector * sector_size;
        assert!(end >= UPDATE_SEQUENCE_WORD_BYTES);
        assert!(end <= record_bytes);
        let word: usize = kani::any();
        kani::assume(word < usl);
        assert!(
            uso + word * UPDATE_SEQUENCE_WORD_BYTES + UPDATE_SEQUENCE_WORD_BYTES <= record_bytes
        );
    }
    kani::cover!(layout.is_some());
    kani::cover!(layout.is_none());
}

#[kani::proof]
#[kani::unwind(2)]
fn invalid_sector_geometry_never_produces_a_layout() {
    let record_bytes: usize = kani::any();
    let sector_size: usize = kani::any();
    kani::assume(
        !(SECTOR..=MAX_SECTOR).contains(&sector_size)
            || !sector_size.is_power_of_two()
            || record_bytes < sector_size,
    );
    assert!(
        checked_fixup_layout(
            record_bytes,
            sector_size,
            kani::any(),
            kani::any(),
            kani::any(),
        )
        .is_none()
    );
    kani::cover!(sector_size == 0);
    kani::cover!(sector_size == MAX_SECTOR && record_bytes < MAX_SECTOR);
}
