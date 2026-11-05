use super::{
    FileLink, Frn, IndexMutationError, ObjectMetadata, RawEntry, VolumeIndex, WalkState, WalkStep,
    excluded_by_attributes, flags, with_flag,
};

const HIDDEN_BIT: u32 = 3;
const SYSTEM_BIT: u32 = 4;

#[kani::proof]
#[kani::unwind(17)]
fn single_link_refresh_updates_every_live_object_row() {
    let root = Frn((1 << 48) | 5);
    let object = Frn((1 << 48) | 100);
    let other = Frn((1 << 48) | 101);
    let mut index = crate::index::VolumeIndexBuilder::root_index("C:", root).unwrap();
    let original = RawEntry {
        parent_frn: root,
        frn: object,
        name_utf16: &[u16::from(b'a')],
        is_dir: false,
        is_reparse: kani::any(),
        is_hidden: kani::any(),
        is_system: kani::any(),
        size: kani::any(),
        mtime: kani::any(),
    };
    index.upsert_link_usn(&original).unwrap();
    index
        .upsert_link_usn(&RawEntry {
            name_utf16: &[u16::from(b'b')],
            ..original
        })
        .unwrap();
    let unrelated = index
        .upsert_link_usn(&RawEntry {
            frn: other,
            name_utf16: &[u16::from(b'd')],
            size: 7,
            mtime: 0,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            ..original
        })
        .unwrap();
    let previous_generation = index.stat_generation;
    let previous_mtime = index.mtime(index.entry_by_frn(object).unwrap());
    let replace_existing: bool = kani::any();
    let name = if replace_existing {
        [u16::from(b'a')]
    } else {
        [u16::from(b'c')]
    };
    let updated = RawEntry {
        name_utf16: &name,
        size: kani::any(),
        mtime: kani::any(),
        is_reparse: kani::any(),
        is_hidden: kani::any(),
        is_system: kani::any(),
        ..original
    };
    let inserted = index.upsert_link_usn(&updated).unwrap();
    let expected_mtime = index.mtime(inserted);
    assert_eq!(
        index.entries_by_frn(object).count(),
        if replace_existing { 2 } else { 3 }
    );
    for id in index.entries_by_frn(object) {
        assert_eq!(index.size(id), updated.size);
        assert_eq!(index.mtime(id), expected_mtime);
        assert_eq!(index.is_reparse(id), updated.is_reparse);
        assert_eq!(
            index.flag[id as usize] & flags::HIDDEN != 0,
            updated.is_hidden
        );
        assert_eq!(
            index.flag[id as usize] & flags::SYSTEM != 0,
            updated.is_system
        );
        assert_eq!(
            index.is_excluded(id),
            updated.is_hidden || updated.is_system
        );
    }
    if original.size != updated.size || previous_mtime != expected_mtime {
        assert!(index.stat_generation > previous_generation);
    }
    assert_eq!(index.size(unrelated), 7);
    assert_eq!(index.mtime(unrelated), 0);
    assert!(!index.is_reparse(unrelated));
    assert!(!index.is_excluded(unrelated));
    assert!(index.is_dir(VolumeIndex::ROOT));
    kani::cover!(replace_existing);
    kani::cover!(!replace_existing);
    kani::cover!(updated.size > u64::from(u32::MAX));
    kani::cover!(updated.size < u64::from(u32::MAX));
}

fn arbitrary_entry() -> RawEntry<'static> {
    RawEntry {
        frn: Frn(kani::any()),
        parent_frn: Frn(kani::any()),
        name_utf16: &[],
        is_dir: kani::any(),
        is_reparse: kani::any(),
        is_hidden: kani::any(),
        is_system: kani::any(),
        size: kani::any(),
        mtime: kani::any(),
    }
}

#[kani::proof]
#[kani::unwind(9)]
fn flag_updates_preserve_unselected_bits_and_are_idempotent() {
    let original: u8 = kani::any();
    let mask: u8 = kani::any();
    let enabled: bool = kani::any();
    let result = with_flag(original, mask, enabled);
    for bit in 0..u8::BITS {
        let selected = 1u8 << bit;
        let expected = if mask & selected != 0 {
            enabled
        } else {
            original & selected != 0
        };
        assert_eq!(result & selected != 0, expected);
    }
    assert_eq!(with_flag(result, mask, enabled), result);
    kani::cover!(enabled && original & mask != mask);
    kani::cover!(!enabled && original & mask != 0);
}

#[kani::proof]
#[kani::unwind(1)]
fn exclusion_uses_only_the_hidden_and_system_bits() {
    let flags: u8 = kani::any();
    let hidden = (flags >> HIDDEN_BIT) & 1 != 0;
    let system = (flags >> SYSTEM_BIT) & 1 != 0;
    assert_eq!(excluded_by_attributes(flags), hidden || system);
    kani::cover!(hidden && !system);
    kani::cover!(!hidden && system);
    kani::cover!(!hidden && !system);
}

#[kani::proof]
#[kani::unwind(1)]
fn file_link_kind_checks_preserve_the_requested_object() {
    let object = Frn(kani::any());
    let entry = arbitrary_entry();
    let result = FileLink::checked(object, &entry);
    let accepted = result.is_ok();
    assert_eq!(accepted, entry.frn == object && !entry.is_dir);
    match result {
        Ok(link) => {
            assert_eq!(link.0.frn, object);
            assert!(!link.0.is_dir);
        }
        Err(error) => assert_eq!(error, IndexMutationError::ObjectKind { object }),
    }
    kani::cover!(accepted);
    kani::cover!(!accepted);
}

#[kani::proof]
#[kani::unwind(1)]
fn metadata_projection_preserves_every_object_field() {
    let first = arbitrary_entry();
    let second = arbitrary_entry();
    let projected = ObjectMetadata::from(&first);
    assert_eq!(projected.size, first.size);
    assert_eq!(projected.mtime, first.mtime);
    assert_eq!(projected.is_reparse, first.is_reparse);
    assert_eq!(projected.is_hidden, first.is_hidden);
    assert_eq!(projected.is_system, first.is_system);
    let equal = first.size == second.size
        && first.mtime == second.mtime
        && first.is_reparse == second.is_reparse
        && first.is_hidden == second.is_hidden
        && first.is_system == second.is_system;
    assert_eq!(projected == ObjectMetadata::from(&second), equal);
    kani::cover!(equal);
    kani::cover!(!equal);
}

#[kani::proof]
#[kani::unwind(4)]
fn each_walk_state_has_one_exhaustive_entry_transition() {
    let mut visited = false;
    let mut cycled = false;
    let mut stopped = false;
    for original in [WalkState::Unseen, WalkState::Visiting, WalkState::Done] {
        let mut state = original;
        let entered = state.enter();
        match original {
            WalkState::Unseen => {
                assert_eq!(entered, WalkStep::Visit);
                assert_eq!(state, WalkState::Visiting);
            }
            WalkState::Visiting => {
                assert_eq!(entered, WalkStep::Cycle);
                assert_eq!(state, original);
            }
            WalkState::Done => {
                assert_eq!(entered, WalkStep::Stop);
                assert_eq!(state, original);
            }
        }
        assert_ne!(state.enter(), WalkStep::Visit);
        visited |= entered == WalkStep::Visit;
        cycled |= entered == WalkStep::Cycle;
        stopped |= entered == WalkStep::Stop;
    }
    kani::cover!(visited);
    kani::cover!(cycled);
    kani::cover!(stopped);
}
