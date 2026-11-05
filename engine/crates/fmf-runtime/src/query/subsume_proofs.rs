use super::{Matcher, implies};

#[kani::proof]
#[kani::unwind(1)]
fn size_and_directory_implication_preserve_membership() {
    let previous_min: u64 = kani::any();
    let previous_max: u64 = kani::any();
    let next_min: u64 = kani::any();
    let next_max: u64 = kani::any();
    let value: u64 = kani::any();
    let previous = Matcher::Size {
        min: previous_min,
        max: previous_max,
    };
    let next = Matcher::Size {
        min: next_min,
        max: next_max,
    };
    let accepted = implies(&next, &previous);
    let member = next_min <= value && value <= next_max;
    if accepted && member {
        assert!(previous_min <= value && value <= previous_max);
    }
    let previous_directory: bool = kani::any();
    let next_directory: bool = kani::any();
    assert_eq!(
        implies(
            &Matcher::IsDir(next_directory),
            &Matcher::IsDir(previous_directory)
        ),
        next_directory == previous_directory,
    );
    kani::cover!(accepted && member);
    kani::cover!(!accepted && member);
    kani::cover!(previous_directory == next_directory);
    kani::cover!(previous_directory != next_directory);
}

#[kani::proof]
#[kani::unwind(1)]
fn mtime_implication_preserves_membership() {
    let previous_min: i64 = kani::any();
    let previous_max: i64 = kani::any();
    let next_min: i64 = kani::any();
    let next_max: i64 = kani::any();
    let value: i64 = kani::any();
    let previous = Matcher::Mtime {
        min: previous_min,
        max: previous_max,
    };
    let next = Matcher::Mtime {
        min: next_min,
        max: next_max,
    };
    let accepted = implies(&next, &previous);
    let member = next_min <= value && value <= next_max;
    if accepted && member {
        assert!(previous_min <= value && value <= previous_max);
    }
    kani::cover!(accepted && member);
    kani::cover!(!accepted && member);
}
