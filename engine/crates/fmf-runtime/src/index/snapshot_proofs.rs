use super::{initialize_column, pod_bytes};

const MAX_COLUMN_ELEMENTS: usize = 4;

fn check_zeroed<T: bytemuck::Pod + Eq, const LENGTH: usize>() -> std::io::Result<usize> {
    let mut column = Vec::<T>::with_capacity(MAX_COLUMN_ELEMENTS);
    initialize_column(&mut column, LENGTH)?;
    assert_eq!(column.len(), LENGTH);
    assert_eq!(pod_bytes(&column).len(), LENGTH * size_of::<T>());
    assert!(column.iter().all(|word| *word == T::zeroed()));
    Ok(column.len())
}

fn check_growth<const LENGTH: usize>() -> std::io::Result<u64> {
    let prefix: [u64; MAX_COLUMN_ELEMENTS] = kani::any();
    let mut column = Vec::from(prefix);
    column.truncate(LENGTH);
    initialize_column(&mut column, MAX_COLUMN_ELEMENTS)?;
    assert_eq!(column.len(), MAX_COLUMN_ELEMENTS);
    for index in 0..LENGTH {
        assert_eq!(column[index], prefix[index]);
    }
    assert!(column[LENGTH..].iter().all(|word| *word == 0));
    Ok(column[0])
}

#[kani::proof]
#[kani::unwind(9)]
fn byte_columns_are_initialized_before_reader_access() {
    let empty = check_zeroed::<u8, 0>().ok();
    assert_eq!(check_zeroed::<u8, 1>().ok(), Some(1));
    assert_eq!(check_zeroed::<u8, 2>().ok(), Some(2));
    assert_eq!(check_zeroed::<u8, 3>().ok(), Some(3));
    let full = check_zeroed::<u8, MAX_COLUMN_ELEMENTS>().ok();
    assert_eq!(empty, Some(0));
    assert_eq!(full, Some(MAX_COLUMN_ELEMENTS));
    kani::cover!(empty == Some(0));
    kani::cover!(full == Some(MAX_COLUMN_ELEMENTS));
}

#[kani::proof]
#[kani::unwind(9)]
fn word_columns_are_initialized_before_reader_access() {
    let empty = check_zeroed::<u32, 0>().ok();
    assert_eq!(check_zeroed::<u32, 1>().ok(), Some(1));
    assert_eq!(check_zeroed::<u32, 2>().ok(), Some(2));
    assert_eq!(check_zeroed::<u32, 3>().ok(), Some(3));
    let full = check_zeroed::<u32, MAX_COLUMN_ELEMENTS>().ok();
    assert_eq!(empty, Some(0));
    assert_eq!(full, Some(MAX_COLUMN_ELEMENTS));
    kani::cover!(empty == Some(0));
    kani::cover!(full == Some(MAX_COLUMN_ELEMENTS));
}

#[kani::proof]
#[kani::unwind(9)]
fn wide_columns_are_initialized_before_reader_access() {
    let empty = check_zeroed::<u64, 0>().ok();
    assert_eq!(check_zeroed::<u64, 1>().ok(), Some(1));
    assert_eq!(check_zeroed::<u64, 2>().ok(), Some(2));
    assert_eq!(check_zeroed::<u64, 3>().ok(), Some(3));
    let full = check_zeroed::<u64, MAX_COLUMN_ELEMENTS>().ok();
    assert_eq!(empty, Some(0));
    assert_eq!(full, Some(MAX_COLUMN_ELEMENTS));
    kani::cover!(empty == Some(0));
    kani::cover!(full == Some(MAX_COLUMN_ELEMENTS));
}

#[kani::proof]
#[kani::unwind(9)]
fn initialized_growth_preserves_arbitrary_existing_column_bytes() {
    let all_new = check_growth::<0>().ok();
    assert!(check_growth::<1>().is_ok());
    assert!(check_growth::<2>().is_ok());
    assert!(check_growth::<3>().is_ok());
    let all_existing = check_growth::<MAX_COLUMN_ELEMENTS>().ok();
    assert_eq!(all_new, Some(0));
    kani::cover!(all_existing == Some(0));
    kani::cover!(all_existing == Some(u64::MAX));
}
