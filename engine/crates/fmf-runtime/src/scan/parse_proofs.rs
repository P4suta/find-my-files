use super::ParsedBatch;

#[kani::proof]
#[kani::unwind(1)]
fn recording_corruption_increments_once_and_preserves_other_batch_counts() {
    let previous: u64 = kani::any();
    kani::assume(previous < u64::MAX);
    let mut batch = ParsedBatch {
        corrupt_records: previous,
        files: kani::any(),
        dirs: kani::any(),
        extension_records: kani::any(),
        skipped_no_name: kani::any(),
        deferred_name_read_failures: kani::any(),
        deferred_stat_failures: kani::any(),
        ..Default::default()
    };
    let other = (
        batch.files,
        batch.dirs,
        batch.extension_records,
        batch.skipped_no_name,
        batch.deferred_name_read_failures,
        batch.deferred_stat_failures,
    );
    batch.record_corruption();
    assert!(batch.corrupt_records > previous);
    assert_eq!(batch.corrupt_records.wrapping_sub(previous), 1);
    assert_eq!(
        (
            batch.files,
            batch.dirs,
            batch.extension_records,
            batch.skipped_no_name,
            batch.deferred_name_read_failures,
            batch.deferred_stat_failures,
        ),
        other
    );
    kani::cover!(previous == 0);
    kani::cover!(previous == u64::MAX - 1);
}
