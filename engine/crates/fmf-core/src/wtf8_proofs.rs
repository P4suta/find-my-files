use super::{
    FOUR_BYTE_WIDTH, THREE_BYTE_WIDTH, TWO_BYTE_WIDTH, completed_output_len, decode_units,
};

const _: () = assert!(FOUR_BYTE_WIDTH <= u8::MAX as usize);

struct DecoderChecks {
    accepted: bool,
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

fn check_length<const LENGTH: usize>() -> DecoderChecks {
    let bytes: [u8; LENGTH] = kani::any();
    let original: [u16; FOUR_BYTE_WIDTH + 1] = kani::any();
    let mut out = original;
    let result = decode_units(&bytes, &mut out[1..LENGTH + 1]);
    let index: u8 = kani::any();
    let index = usize::from(index);
    kani::assume(index <= FOUR_BYTE_WIDTH);
    let valid_offset = result.is_ok() || result.is_err_and(|error| error.offset < LENGTH);
    let untouched = result.is_err() || result.is_ok_and(|written| index > written);
    let atomic_rejection = valid_offset && (!untouched || out[index] == original[index]);
    DecoderChecks {
        accepted: result.is_ok(),
        prefix_preserved: out[0] == original[0],
        bounded: result.is_err() || result.is_ok_and(|written| written <= LENGTH),
        atomic_rejection,
    }
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
#[kani::solver(cadical)]
fn two_byte_decoding_preserves_prefix_or_rolls_back() {
    let checked = check_length::<TWO_BYTE_WIDTH>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
fn three_byte_decoding_preserves_prefix_or_rolls_back() {
    let checked = check_length::<THREE_BYTE_WIDTH>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
fn four_byte_decoding_preserves_prefix_or_rolls_back() {
    let checked = check_length::<FOUR_BYTE_WIDTH>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}
