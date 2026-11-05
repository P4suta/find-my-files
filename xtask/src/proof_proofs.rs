use super::{address_space_is_bounded, Shard, Summary, MAX_ADDRESS_SPACE_BYTES, MAX_SHARDS};

#[kani::proof]
#[kani::unwind(2)]
fn process_address_space_limits_are_positive_and_bounded() {
    let bytes: u64 = kani::any();
    let accepted = address_space_is_bounded(bytes);
    if accepted {
        assert!(bytes > 0);
        assert!(bytes <= MAX_ADDRESS_SPACE_BYTES);
    } else {
        assert!(bytes == 0 || bytes > MAX_ADDRESS_SPACE_BYTES);
    }
    kani::cover!(accepted && bytes == MAX_ADDRESS_SPACE_BYTES);
    kani::cover!(!accepted && bytes == 0);
}

#[kani::proof]
#[kani::unwind(5)]
fn every_ordinal_has_exactly_one_partition_owner() {
    let ordinal: usize = kani::any();
    let count: usize = kani::any();
    kani::assume((1..=MAX_SHARDS).contains(&count));
    let mut owners = 0;
    for index in 0..MAX_SHARDS {
        if index < count {
            let shard = Shard { index, count };
            if shard.owns(ordinal) {
                assert!(index < count);
                assert_eq!(ordinal % count, index);
                owners += 1;
            }
        }
    }
    assert_eq!(owners, 1);
    kani::cover!(count == 1 && ordinal == usize::MAX);
    kani::cover!(count == 4 && ordinal == usize::MAX);
}

#[kani::proof]
#[kani::unwind(2)]
fn accepted_summary_requires_every_registered_harness_to_pass() {
    let summary = Summary {
        total_harnesses: kani::any(),
        executed: kani::any(),
        successful: kani::any(),
        failed: kani::any(),
    };
    let expected: usize = kani::any();
    let complete = summary.complete(expected);
    if complete {
        assert!(expected > 0);
        assert_eq!(summary.total_harnesses, expected);
        assert_eq!(summary.executed, expected);
        assert_eq!(summary.successful, expected);
        assert_eq!(summary.failed, 0);
    }
    if expected == 0 {
        assert!(!complete);
    }
    kani::cover!(complete);
    kani::cover!(!complete);
}

#[kani::proof]
#[kani::unwind(2)]
fn a_complete_nonempty_summary_remains_acceptable() {
    let expected: usize = kani::any();
    kani::assume(expected > 0);
    let summary = Summary {
        total_harnesses: expected,
        executed: expected,
        successful: expected,
        failed: 0,
    };
    assert!(summary.complete(expected));
    kani::cover!(expected == 1);
    kani::cover!(expected == usize::MAX);
}
