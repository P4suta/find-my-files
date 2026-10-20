use super::{BUCKET_COUNT, BucketIndex, Histogram};

#[kani::proof]
#[kani::unwind(33)]
fn recording_changes_only_the_selected_bucket_without_overflow() {
    let buckets: [u64; BUCKET_COUNT] = kani::any();
    let count: u64 = kani::any();
    let sum_us: u64 = kani::any();
    let maximum: u64 = kani::any();
    let sample: u64 = kani::any();
    let mut histogram = Histogram {
        buckets,
        count,
        sum_us,
        max_us: maximum,
    };
    histogram.record(sample);
    assert_eq!(histogram.count, count.checked_add(1).unwrap_or(u64::MAX));
    assert_eq!(
        histogram.sum_us,
        sum_us.checked_add(sample).unwrap_or(u64::MAX)
    );
    assert_eq!(histogram.max_us, maximum.max(sample));
    let selected = BucketIndex::for_micros(sample).index();
    for (index, previous) in buckets.into_iter().enumerate() {
        if index == selected {
            assert_eq!(
                histogram.buckets[index],
                previous.checked_add(1).unwrap_or(u64::MAX)
            );
        } else {
            assert_eq!(histogram.buckets[index], previous);
        }
    }
    kani::cover!(count == 0 && sample == 0);
    kani::cover!(count == u64::MAX && sample == u64::MAX);
}
