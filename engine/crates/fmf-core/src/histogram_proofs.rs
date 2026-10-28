use super::{BUCKET_COUNT, BucketIndex};

#[kani::proof]
#[kani::unwind(1)]
fn slots_are_always_bounded_and_preserve_every_valid_index() {
    let slot: usize = kani::any();
    let index = BucketIndex::from_slot_clamped(slot).index();
    assert!(index < BUCKET_COUNT);
    if slot < BUCKET_COUNT {
        assert_eq!(index, slot);
    } else {
        assert_eq!(index, BUCKET_COUNT - 1);
    }
    kani::cover!(slot == 0);
    kani::cover!(slot == usize::MAX);
}

#[kani::proof]
#[kani::unwind(1)]
fn samples_have_a_bounded_bucket_and_a_conservative_upper_bound() {
    let sample: u64 = kani::any();
    let bucket = BucketIndex::for_micros(sample);
    let index = bucket.index();
    assert!(index < BUCKET_COUNT);
    assert!(sample.max(1) >= 1_u64 << index);
    if index + 1 < BUCKET_COUNT {
        assert!(sample.max(1) < 1_u64 << (index + 1));
    }
    assert!(bucket.upper_bound(sample) >= sample);
    kani::cover!(sample == 0);
    kani::cover!(sample == u64::MAX);
}
