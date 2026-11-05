use super::{ChangeKind, ChangeReasons};

#[kani::proof]
#[kani::unwind(1)]
fn every_reason_bit_preserves_independent_effects_and_operation_priority() {
    let bits: u32 = kani::any();
    let reasons = ChangeReasons::from_bits(bits);
    let overwrite = bits % 2 == 1;
    let extend = (bits >> 1) % 2 == 1;
    let truncate = (bits >> 2) % 2 == 1;
    let create = (bits >> 8) % 2 == 1;
    let delete = (bits >> 9) % 2 == 1;
    let old_name = (bits >> 12) % 2 == 1;
    let new_name = (bits >> 13) % 2 == 1;
    let basic = (bits >> 15) % 2 == 1;
    let hard_link = (bits >> 16) % 2 == 1;
    let reparse = (bits >> 20) % 2 == 1;
    let expected = if delete {
        ChangeKind::Delete
    } else if create || new_name {
        ChangeKind::Upsert
    } else if overwrite || extend || truncate || basic || reparse {
        ChangeKind::Metadata
    } else {
        ChangeKind::Ignore
    };
    assert_eq!(reasons.kind(), expected);
    assert_eq!(reasons.has_old_name(), old_name);
    assert_eq!(reasons.creates(), create);
    assert_eq!(reasons.renames(), new_name);
    assert_eq!(reasons.changes_hard_links(), hard_link);
    assert_eq!(reasons.changes_links(), create || new_name || hard_link);
    let complete: bool = kani::any();
    let ambiguous: bool = kani::any();
    assert_eq!(
        reasons.rejects_link_snapshot(complete, ambiguous),
        !complete && (hard_link || ambiguous)
    );
    kani::cover!(matches!(expected, ChangeKind::Ignore));
    kani::cover!(matches!(expected, ChangeKind::Delete));
    kani::cover!(matches!(expected, ChangeKind::Upsert));
    kani::cover!(matches!(expected, ChangeKind::Metadata));
}

#[kani::proof]
#[kani::unwind(1)]
fn reason_accumulation_preserves_both_inputs_and_repeated_flags() {
    let left: u32 = kani::any();
    let right: u32 = kani::any();
    let left = ChangeReasons::from_bits(left);
    let right = ChangeReasons::from_bits(right);
    let union = left.union(right);
    assert_eq!(union.0, left.0 | right.0);
    assert_eq!(left.union(left), left);
    assert_eq!(union, right.union(left));
    kani::cover!(left == right && left.0 != 0);
    kani::cover!(left != right);
}
