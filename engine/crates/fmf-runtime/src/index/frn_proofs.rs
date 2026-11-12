use super::{EntryId, Frn, FrnIndex, flags};

const ROWS: usize = 4;
const SORTED_IDS: [EntryId; ROWS] = [0, 1, 2, 3];
const SORTED_ROWS: [usize; ROWS] = [0, 1, 2, 3];
const TAIL_ROWS: [usize; ROWS] = [3, 2, 1, 0];

fn check_hint(rows: &impl Iterator<Item = EntryId>, remaining: usize) {
    let (lower, upper) = rows.size_hint();
    assert!(lower <= remaining);
    assert!(upper.is_none_or(|upper| remaining <= upper));
}

fn check_lookup<const COVERS: usize>() -> (usize, bool) {
    let frn: [u64; ROWS] = kani::any();
    let flag: [u8; ROWS] = kani::any();
    let covers = COVERS;
    for id in 1..covers {
        kani::assume(Frn(frn[id - 1]).record() <= Frn(frn[id]).record());
    }
    let ids = SORTED_IDS;
    let index = FrnIndex {
        ids: ids[..covers].to_vec(),
        covers: covers as u32,
    };
    let key = Frn(kani::any()).record();
    let mut expected = [u32::MAX; ROWS];
    let mut count = 0;
    for id in TAIL_ROWS {
        if id >= covers && Frn(frn[id]).record() == key && flag[id] & flags::TOMBSTONE == 0 {
            expected[count] = id as u32;
            count += 1;
        }
    }
    for id in SORTED_ROWS {
        if id < covers && Frn(frn[id]).record() == key && flag[id] & flags::TOMBSTONE == 0 {
            expected[count] = id as u32;
            count += 1;
        }
    }
    let mut rows = index.lookup_all(key, &frn, &flag);
    check_hint(&rows, count);
    for position in 0..ROWS {
        if position < count {
            assert_eq!(rows.next(), Some(expected[position]));
            check_hint(&rows, count - position - 1);
        } else {
            assert_eq!(rows.next(), None);
            check_hint(&rows, 0);
        }
    }
    assert_eq!(rows.next(), None);
    assert_eq!(rows.next(), None);
    check_hint(&rows, 0);
    (count, flag[0] & flags::TOMBSTONE != 0)
}

#[kani::proof]
#[kani::unwind(6)]
fn lookup_with_no_sorted_rows_preserves_order_liveness_and_size_hints() {
    let (count, first_deleted) = check_lookup::<0>();
    assert!(count <= ROWS);
    kani::cover!(count == ROWS);
    kani::cover!(first_deleted && count > 0);
    kani::cover!(count == 0);
}

#[kani::proof]
#[kani::unwind(6)]
fn lookup_with_one_sorted_row_preserves_order_liveness_and_size_hints() {
    let (count, first_deleted) = check_lookup::<1>();
    assert!(count <= ROWS);
    kani::cover!(count == ROWS);
    kani::cover!(first_deleted && count > 0);
    kani::cover!(count == 0);
}

#[kani::proof]
#[kani::unwind(6)]
fn lookup_with_two_sorted_rows_preserves_order_liveness_and_size_hints() {
    let (count, first_deleted) = check_lookup::<2>();
    assert!(count <= ROWS);
    kani::cover!(count == ROWS);
    kani::cover!(first_deleted && count > 0);
    kani::cover!(count == 0);
}

#[kani::proof]
#[kani::unwind(6)]
fn lookup_with_three_sorted_rows_preserves_order_liveness_and_size_hints() {
    let (count, first_deleted) = check_lookup::<3>();
    assert!(count <= ROWS);
    kani::cover!(count == ROWS);
    kani::cover!(first_deleted && count > 0);
    kani::cover!(count == 0);
}

#[kani::proof]
#[kani::unwind(6)]
fn lookup_with_four_sorted_rows_preserves_order_liveness_and_size_hints() {
    let (count, first_deleted) = check_lookup::<ROWS>();
    assert!(count <= ROWS);
    kani::cover!(count == ROWS);
    kani::cover!(first_deleted && count > 0);
    kani::cover!(count == 0);
}
