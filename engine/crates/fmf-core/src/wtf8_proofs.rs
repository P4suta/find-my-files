use super::{
    FOUR_BYTE_WIDTH, THREE_BYTE_WIDTH, TWO_BYTE_WIDTH, completed_output_len, decode_units,
};

struct DecoderChecks {
    accepted: bool,
    prefix_preserved: bool,
    bounded: bool,
    atomic_rejection: bool,
}

fn check_length<const LENGTH: usize>() -> DecoderChecks {
    let bytes: [u8; LENGTH] = kani::any();
    let original: [u16; FOUR_BYTE_WIDTH + 1] = kani::any();
    let mut out = original;
    let result = decode_units(&bytes, &mut out[1..LENGTH + 1]);
    let mut atomic_rejection = result.is_ok() || result.is_err_and(|error| error.offset < LENGTH);
    for index in 0..=FOUR_BYTE_WIDTH {
        if result.is_err() || result.is_ok_and(|written| index > written) {
            atomic_rejection &= out[index] == original[index];
        }
    }
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
fn two_byte_decoding_preserves_prefix_or_rolls_back() {
    let checked = check_length::<TWO_BYTE_WIDTH>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn three_byte_decoding_preserves_prefix_or_rolls_back() {
    let checked = check_length::<THREE_BYTE_WIDTH>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}

#[kani::proof]
#[kani::unwind(6)]
fn four_byte_decoding_preserves_prefix_or_rolls_back() {
    let checked = check_length::<FOUR_BYTE_WIDTH>();
    assert!(checked.prefix_preserved && checked.bounded && checked.atomic_rejection);
    kani::cover!(checked.accepted);
    kani::cover!(!checked.accepted);
}
