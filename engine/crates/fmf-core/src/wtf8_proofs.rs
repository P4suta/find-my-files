use super::{
    FOUR_BYTE_WIDTH, THREE_BYTE_WIDTH, TWO_BYTE_WIDTH, completed_output_len, decode_units,
};

struct DecoderChecks {
    accepted: bool,
    first_byte: Option<u8>,
    failure_offset: Option<usize>,
    prefix_preserved: bool,
    bounded: bool,
    atomic_rejection: bool,
}

#[kani::proof]
#[kani::unwind(6)]
fn accepted_points_consume_input_and_use_the_canonical_width() {
    let bytes: [u8; FOUR_BYTE_WIDTH] = kani::any();
    let length: usize = kani::any();
    let offset: usize = kani::any();
    kani::assume(offset <= length && length <= bytes.len());
    let point = super::decode_point(&bytes[..length], offset);
    if let Ok((point, width)) = point {
        let width: usize = width.into();
        assert!(width > 0 && width <= length - offset);
        assert!(point <= super::MAX_CODE_POINT);
        assert_eq!(width, super::utf8_len(point));
        let sequence = &bytes[offset..offset + width];
        let lead_mask = match width {
            1 => 0x7F,
            2 => 0x1F,
            3 => 0x0F,
            4 => 0x07,
            _ => 0,
        };
        let mut reference = u32::from(sequence[0] & lead_mask);
        for continuation in &sequence[1..] {
            reference = (reference << 6) | u32::from(continuation & 0x3F);
        }
        assert_eq!(point, reference);
    }
    kani::cover!(point.is_ok_and(|(_, width)| usize::from(width) == FOUR_BYTE_WIDTH));
    kani::cover!(point.is_err());
}

#[kani::proof]
#[kani::unwind(4)]
fn surrogate_pairs_encode_exactly_one_supplementary_scalar() {
    let high: u16 = kani::any();
    let low: u16 = kani::any();
    let point = super::surrogate_code_point(high, low);
    let paired = (super::HIGH_SURROGATE_START..=super::HIGH_SURROGATE_END).contains(&high)
        && (super::LOW_SURROGATE_START..=super::LOW_SURROGATE_END).contains(&low);
    assert_eq!(point.is_some(), paired);
    if let Some(point) = point {
        assert!((super::BMP_LIMIT..=super::MAX_CODE_POINT).contains(&point));
        let mut decoded = char::decode_utf16([high, low]);
        assert_eq!(
            decoded.next().and_then(Result::ok).map(u32::from),
            Some(point)
        );
        assert!(decoded.next().is_none());
    }
    kani::cover!(point.is_some());
    kani::cover!(point.is_none());
}

fn check_bytes<const LENGTH: usize>(bytes: &[u8; LENGTH]) -> DecoderChecks {
    let original: [u16; FOUR_BYTE_WIDTH + 1] = kani::any();
    let mut out = original;
    let result = decode_units(bytes, &mut out[1..LENGTH + 1]);
    let mut atomic_rejection = result.is_ok() || result.is_err_and(|error| error.offset < LENGTH);
    for index in 0..=FOUR_BYTE_WIDTH {
        if result.is_err() || result.is_ok_and(|written| index > written) {
            atomic_rejection &= out[index] == original[index];
        }
    }
    DecoderChecks {
        accepted: result.is_ok(),
        first_byte: bytes.first().copied(),
        failure_offset: result.err().map(|error| error.offset),
        prefix_preserved: out[0] == original[0],
        bounded: result.is_err() || result.is_ok_and(|written| written <= LENGTH),
        atomic_rejection,
    }
}

fn check_length<const LENGTH: usize>() -> DecoderChecks {
    let bytes: [u8; LENGTH] = kani::any();
    check_bytes(&bytes)
}

const PARTITION_ASCII_LAST: u8 = 0x7F;
const PARTITION_TWO_BYTE_LEAD_START: u8 = 0xC2;
const PARTITION_TWO_BYTE_LEAD_END: u8 = 0xDF;
const PARTITION_THREE_BYTE_LEAD_START: u8 = 0xE0;
const PARTITION_THREE_BYTE_LEAD_END: u8 = 0xEF;
const PARTITION_FOUR_BYTE_LEAD_START: u8 = 0xF0;
const PARTITION_FOUR_BYTE_LEAD_END: u8 = 0xF4;
const ASCII_PAIR: [u8; FOUR_BYTE_WIDTH] = [1, 1, 0, 0];
const SINGLE_TWO_BYTE_POINT: [u8; FOUR_BYTE_WIDTH] = [2, 0, 0, 0];
const ASCII_TRIPLE: [u8; FOUR_BYTE_WIDTH] = [1, 1, 1, 0];
const ASCII_THEN_TWO_BYTE: [u8; FOUR_BYTE_WIDTH] = [1, 2, 0, 0];
const TWO_BYTE_THEN_ASCII: [u8; FOUR_BYTE_WIDTH] = [2, 1, 0, 0];
const SINGLE_THREE_BYTE_POINT: [u8; FOUR_BYTE_WIDTH] = [3, 0, 0, 0];
const ASCII_QUAD: [u8; FOUR_BYTE_WIDTH] = [1, 1, 1, 1];
const TWO_ASCII_THEN_TWO_BYTE: [u8; FOUR_BYTE_WIDTH] = [1, 1, 2, 0];
const ASCII_TWO_BYTE_ASCII: [u8; FOUR_BYTE_WIDTH] = [1, 2, 1, 0];
const TWO_BYTE_THEN_TWO_ASCII: [u8; FOUR_BYTE_WIDTH] = [2, 1, 1, 0];
const TWO_TWO_BYTE_POINTS: [u8; FOUR_BYTE_WIDTH] = [2, 2, 0, 0];
const ASCII_THEN_THREE_BYTE: [u8; FOUR_BYTE_WIDTH] = [1, 3, 0, 0];
const THREE_BYTE_THEN_ASCII: [u8; FOUR_BYTE_WIDTH] = [3, 1, 0, 0];
const SINGLE_FOUR_BYTE_POINT: [u8; FOUR_BYTE_WIDTH] = [4, 0, 0, 0];

fn wire_widths(bytes: &[u8]) -> Option<[u8; FOUR_BYTE_WIDTH]> {
    let mut widths = [0; FOUR_BYTE_WIDTH];
    let mut offset = 0;
    let mut count = 0;
    while let Some(&lead) = bytes.get(offset) {
        let width = match lead {
            0..=PARTITION_ASCII_LAST => 1,
            PARTITION_TWO_BYTE_LEAD_START..=PARTITION_TWO_BYTE_LEAD_END => TWO_BYTE_WIDTH as u8,
            PARTITION_THREE_BYTE_LEAD_START..=PARTITION_THREE_BYTE_LEAD_END => {
                THREE_BYTE_WIDTH as u8
            }
            PARTITION_FOUR_BYTE_LEAD_START..=PARTITION_FOUR_BYTE_LEAD_END => FOUR_BYTE_WIDTH as u8,
            _ => return None,
        };
        if usize::from(width) > bytes.len() - offset {
            return None;
        }
        *widths.get_mut(count)? = width;
        count += 1;
        offset += usize::from(width);
    }
    Some(widths)
}

fn known_widths<const LENGTH: usize>(widths: Option<[u8; FOUR_BYTE_WIDTH]>) -> bool {
    match LENGTH {
        TWO_BYTE_WIDTH => matches!(widths, Some(ASCII_PAIR | SINGLE_TWO_BYTE_POINT)),
        THREE_BYTE_WIDTH => matches!(
            widths,
            Some(
                ASCII_TRIPLE | ASCII_THEN_TWO_BYTE | TWO_BYTE_THEN_ASCII | SINGLE_THREE_BYTE_POINT
            )
        ),
        FOUR_BYTE_WIDTH => matches!(
            widths,
            Some(
                ASCII_QUAD
                    | TWO_ASCII_THEN_TWO_BYTE
                    | ASCII_TWO_BYTE_ASCII
                    | TWO_BYTE_THEN_TWO_ASCII
                    | TWO_TWO_BYTE_POINTS
                    | ASCII_THEN_THREE_BYTE
                    | THREE_BYTE_THEN_ASCII
                    | SINGLE_FOUR_BYTE_POINT
            )
        ),
        _ => false,
    }
}

fn check_widths<const LENGTH: usize>(widths: [u8; FOUR_BYTE_WIDTH]) -> DecoderChecks {
    let bytes: [u8; LENGTH] = kani::any();
    kani::assume(wire_widths(&bytes) == Some(widths));
    check_bytes(&bytes)
}

fn check_invalid_widths<const LENGTH: usize>() -> DecoderChecks {
    let bytes: [u8; LENGTH] = kani::any();
    kani::assume(wire_widths(&bytes).is_none());
    check_bytes(&bytes)
}

#[kani::proof]
#[kani::unwind(6)]
fn empty_and_single_byte_decoding_preserve_prefix_or_roll_back() {
    let empty = check_length::<0>();
    assert!(empty.accepted && empty.prefix_preserved && empty.bounded);
    let checked = check_length::<1>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(1)]
fn output_lengths_preserve_the_prefix_or_reject_overflow() {
    let initial: usize = kani::any();
    let written: usize = kani::any();
    let completed = completed_output_len(initial, Some(written));
    match completed {
        Some(length) => {
            assert!(length >= initial && length - initial == written);
            assert!(written <= usize::MAX - initial);
        }
        None => assert!(written > usize::MAX - initial),
    }
    assert_eq!(completed_output_len(initial, None), Some(initial));
    kani::cover!(completed.is_some());
    kani::cover!(completed.is_none());
}

#[kani::proof]
#[kani::unwind(6)]
fn two_byte_width_partitions_cover_every_input() {
    let bytes: [u8; TWO_BYTE_WIDTH] = kani::any();
    let widths = wire_widths(&bytes);
    assert!(widths.is_none() || known_widths::<TWO_BYTE_WIDTH>(widths));
    kani::cover!(widths.is_none());
    kani::cover!(widths == Some(ASCII_PAIR));
}

#[kani::proof]
#[kani::unwind(6)]
fn two_byte_ascii_pair_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<TWO_BYTE_WIDTH>(ASCII_PAIR);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    assert!(checked.accepted);
    kani::cover!(checked.first_byte == Some(0));
    kani::cover!(checked.first_byte == Some(PARTITION_ASCII_LAST));
}

#[kani::proof]
#[kani::unwind(6)]
fn two_byte_single_two_byte_point_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<TWO_BYTE_WIDTH>(SINGLE_TWO_BYTE_POINT);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn two_byte_invalid_widths_preserve_output() {
    let checked = check_invalid_widths::<TWO_BYTE_WIDTH>();
    assert!(!checked.accepted);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.failure_offset == Some(0));
    kani::cover!(checked.failure_offset == Some(TWO_BYTE_WIDTH - 1));
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_width_partitions_cover_every_input() {
    let bytes: [u8; THREE_BYTE_WIDTH] = kani::any();
    let widths = wire_widths(&bytes);
    assert!(widths.is_none() || known_widths::<THREE_BYTE_WIDTH>(widths));
    kani::cover!(widths.is_none());
    kani::cover!(widths == Some(ASCII_TRIPLE));
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_ascii_triple_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<THREE_BYTE_WIDTH>(ASCII_TRIPLE);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    assert!(checked.accepted);
    kani::cover!(checked.first_byte == Some(0));
    kani::cover!(checked.first_byte == Some(PARTITION_ASCII_LAST));
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_ascii_then_two_byte_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<THREE_BYTE_WIDTH>(ASCII_THEN_TWO_BYTE);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_two_byte_then_ascii_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<THREE_BYTE_WIDTH>(TWO_BYTE_THEN_ASCII);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_single_three_byte_point_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<THREE_BYTE_WIDTH>(SINGLE_THREE_BYTE_POINT);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_invalid_widths_preserve_output() {
    let checked = check_invalid_widths::<THREE_BYTE_WIDTH>();
    assert!(!checked.accepted);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.failure_offset == Some(0));
    kani::cover!(checked.failure_offset == Some(THREE_BYTE_WIDTH - 1));
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_width_partitions_cover_every_input() {
    let bytes: [u8; FOUR_BYTE_WIDTH] = kani::any();
    let widths = wire_widths(&bytes);
    assert!(widths.is_none() || known_widths::<FOUR_BYTE_WIDTH>(widths));
    kani::cover!(widths.is_none());
    kani::cover!(widths == Some(ASCII_QUAD));
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_ascii_quad_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(ASCII_QUAD);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    assert!(checked.accepted);
    kani::cover!(checked.first_byte == Some(0));
    kani::cover!(checked.first_byte == Some(PARTITION_ASCII_LAST));
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_two_ascii_then_two_byte_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(TWO_ASCII_THEN_TWO_BYTE);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_ascii_two_byte_ascii_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(ASCII_TWO_BYTE_ASCII);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_two_byte_then_two_ascii_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(TWO_BYTE_THEN_TWO_ASCII);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_two_two_byte_points_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(TWO_TWO_BYTE_POINTS);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_ascii_then_three_byte_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(ASCII_THEN_THREE_BYTE);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_three_byte_then_ascii_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(THREE_BYTE_THEN_ASCII);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_single_four_byte_point_preserves_prefix_or_rolls_back() {
    let checked = check_widths::<FOUR_BYTE_WIDTH>(SINGLE_FOUR_BYTE_POINT);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_invalid_widths_preserve_output() {
    let checked = check_invalid_widths::<FOUR_BYTE_WIDTH>();
    assert!(!checked.accepted);
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.failure_offset == Some(0));
    kani::cover!(checked.failure_offset == Some(FOUR_BYTE_WIDTH - 1));
}
