use super::{encoding_matches, mtime_secs_to_ticks, mtime_ticks_to_secs};

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn stored_seconds_decode_totally_and_deterministically() {
    let seconds: u32 = kani::any();
    let first = mtime_secs_to_ticks(seconds);
    let second = mtime_secs_to_ticks(seconds);
    assert_eq!(first, second);
    assert_eq!(first == 0, seconds == 0);
    kani::cover!(seconds == 0);
    kani::cover!(seconds == u32::MAX);
}

/// Encoded results for two timestamps with equal-input congruence.
pub struct EncodedTimePair {
    first_ticks: i64,
    second_ticks: i64,
    first_seconds: u32,
    second_seconds: u32,
}

impl EncodedTimePair {
    /// Equal timestamps share the first result; distinct timestamps retain both results.
    pub const fn new(
        first_ticks: i64,
        second_ticks: i64,
        first_seconds: u32,
        second_seconds: u32,
    ) -> Self {
        Self {
            first_ticks,
            second_ticks,
            first_seconds,
            second_seconds: if first_ticks == second_ticks {
                first_seconds
            } else {
                second_seconds
            },
        }
    }

    /// Return the registered result, or reject a timestamp outside the pair.
    pub const fn resolve(&self, ticks: i64) -> Option<u32> {
        if ticks == self.first_ticks {
            Some(self.first_seconds)
        } else if ticks == self.second_ticks {
            Some(self.second_seconds)
        } else {
            None
        }
    }
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn a_time_pair_resolves_both_registered_inputs_and_rejects_every_other_input() {
    let first: i64 = kani::any();
    let second: i64 = kani::any();
    let first_seconds: u32 = kani::any();
    let second_seconds: u32 = kani::any();
    let query: i64 = kani::any();
    let pair = EncodedTimePair::new(first, second, first_seconds, second_seconds);
    assert!(pair.resolve(first) == Some(first_seconds));
    assert!(
        pair.resolve(second)
            == Some(if first == second {
                first_seconds
            } else {
                second_seconds
            })
    );
    assert!(pair.resolve(query).is_some() == (query == first || query == second));
    kani::cover!(first == second && query == first);
    kani::cover!(first != second && query == second);
    kani::cover!(query != first && query != second);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn equal_timestamp_results_refine_the_two_input_projection() {
    let ticks: i64 = kani::any();
    let first_seconds = mtime_ticks_to_secs(ticks);
    let second_seconds = mtime_ticks_to_secs(ticks);
    let pair = EncodedTimePair::new(ticks, ticks, first_seconds, second_seconds);
    assert!(pair.resolve(ticks) == Some(first_seconds));
    assert!(pair.resolve(ticks) == Some(second_seconds));
    kani::cover!(ticks == i64::MIN);
    kani::cover!(ticks == i64::MAX);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn distinct_timestamp_results_refine_the_two_input_projection() {
    let first: i64 = kani::any();
    let second: i64 = kani::any();
    kani::assume(first != second);
    let first_seconds = mtime_ticks_to_secs(first);
    let second_seconds = mtime_ticks_to_secs(second);
    let pair = EncodedTimePair::new(first, second, first_seconds, second_seconds);
    assert!(pair.resolve(first) == Some(first_seconds));
    assert!(pair.resolve(second) == Some(second_seconds));
    kani::cover!(first == i64::MIN && second == i64::MAX);
    kani::cover!(first == i64::MAX && second == i64::MIN);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn checked_arithmetic_preserves_value_and_overflow_fields() {
    let first: i8 = kani::any();
    let second: i8 = kani::any();
    let wide_first = i16::from(first);
    let wide_second = i16::from(second);
    let sum = wide_first.wrapping_add(wide_second);
    let difference = wide_first.wrapping_sub(wide_second);
    let product = wide_first.wrapping_mul(wide_second);
    assert_eq!(
        first.overflowing_add(second),
        (
            sum as i8,
            sum < i16::from(i8::MIN) || sum > i16::from(i8::MAX)
        )
    );
    assert_eq!(
        first.overflowing_sub(second),
        (
            difference as i8,
            difference < i16::from(i8::MIN) || difference > i16::from(i8::MAX)
        )
    );
    assert_eq!(
        first.overflowing_mul(second),
        (
            product as i8,
            product < i16::from(i8::MIN) || product > i16::from(i8::MAX)
        )
    );
    let unsigned_first = first as u8;
    let unsigned_second = second as u8;
    let unsigned_product = u16::from(unsigned_first).wrapping_mul(u16::from(unsigned_second));
    assert_eq!(
        unsigned_first.overflowing_mul(unsigned_second),
        (
            unsigned_product as u8,
            unsigned_product > u16::from(u8::MAX)
        )
    );
    kani::cover!(first == i8::MIN && second == -1);
    kani::cover!(first == 1 && second == 2);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn encoding_preserves_clamped_unix_seconds() {
    let ticks: i64 = kani::any();
    let seconds = mtime_ticks_to_secs(ticks);
    assert!(encoding_matches(ticks, seconds));
    kani::cover!(ticks == i64::MIN);
    kani::cover!(ticks == i64::MAX);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn every_tick_value_has_at_most_one_encoded_second() {
    let ticks: i64 = kani::any();
    let first: u32 = kani::any();
    let second: u32 = kani::any();
    kani::assume(encoding_matches(ticks, first));
    kani::assume(encoding_matches(ticks, second));
    assert_eq!(first, second);
    kani::cover!(first == 0 && ticks == i64::MIN);
    kani::cover!(first == u32::MAX && ticks == i64::MAX);
}
