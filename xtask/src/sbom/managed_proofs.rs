#[kani::proof]
#[kani::unwind(2)]
fn accepted_table_ranges_are_contained_without_arithmetic_overflow() {
    let start: usize = kani::any();
    let rows: usize = kani::any();
    let width: usize = kani::any();
    let total: usize = kani::any();
    let range = super::checked_table_range(start, rows, width, total);
    if let Some(ref range) = range {
        assert!(width != 0);
        assert_eq!(range.start, start);
        assert!(range.start <= range.end && range.end <= total);
        assert_eq!(range.end - range.start, rows * width);
    } else if width != 0 {
        assert!(rows
            .checked_mul(width)
            .and_then(|size| start.checked_add(size))
            .is_none_or(|end| end > total));
    }
    kani::cover!(rows == 0 && range.is_some());
    kani::cover!(rows != 0 && range.is_some());
    kani::cover!(range.is_none());
}

#[kani::proof]
#[kani::unwind(5)]
fn accepted_compressed_integers_consume_exactly_the_canonical_prefix() {
    let bytes: [u8; 4] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= bytes.len());
    if let Some((value, consumed)) = super::compressed_prefix(&bytes[..length]) {
        assert!(value <= 0x1fff_ffff);
        assert!(consumed <= length);
        let expected = match consumed {
            1 => {
                assert!(bytes[0] < 0x80);
                u32::from(bytes[0])
            }
            2 => {
                assert!((0x80..=0xbf).contains(&bytes[0]) && value >= 0x80);
                (u32::from(bytes[0] & 0x3f) << 8) | u32::from(bytes[1])
            }
            4 => {
                assert!((0xc0..=0xdf).contains(&bytes[0]) && value >= 0x4000);
                (u32::from(bytes[0] & 0x1f) << 24)
                    | (u32::from(bytes[1]) << 16)
                    | (u32::from(bytes[2]) << 8)
                    | u32::from(bytes[3])
            }
            _ => panic!("accepted a compressed integer with an invalid width"),
        };
        assert_eq!(value, expected);
        kani::cover!(consumed == 1);
        kani::cover!(consumed == 2);
        kani::cover!(consumed == 4);
    } else {
        kani::cover!(length == 0);
        kani::cover!(length == 4);
    }
}
