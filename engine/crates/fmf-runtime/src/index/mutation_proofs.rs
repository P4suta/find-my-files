use super::super::overflow_model::{MetadataAspect, ROOT_FRN, SHARED_FILE_FRN};
use super::{
    FileLink, Frn, IndexMutationError, ObjectMetadata, RawEntry, WalkState, WalkStep,
    excluded_by_attributes, with_flag,
};

const HIDDEN_BIT: u32 = 3;
const SYSTEM_BIT: u32 = 4;

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

struct TimeProjectionState {
    first_ticks: AtomicI64,
    second_ticks: AtomicI64,
    first_seconds: AtomicU32,
    second_seconds: AtomicU32,
    _identity: [u8; 8],
}

static TIME_PROJECTION: TimeProjectionState = TimeProjectionState {
    first_ticks: AtomicI64::new(0),
    second_ticks: AtomicI64::new(0),
    first_seconds: AtomicU32::new(0),
    second_seconds: AtomicU32::new(0),
    _identity: *b"FMFtime!",
};

fn prepare_time_pair(first: i64, second: i64, first_seconds: u32, second_seconds: u32) {
    TIME_PROJECTION.first_ticks.store(first, Ordering::Relaxed);
    TIME_PROJECTION
        .second_ticks
        .store(second, Ordering::Relaxed);
    TIME_PROJECTION
        .first_seconds
        .store(first_seconds, Ordering::Relaxed);
    TIME_PROJECTION
        .second_seconds
        .store(second_seconds, Ordering::Relaxed);
}

fn projected_mtime(ticks: i64) -> u32 {
    let pair = fmf_core::dates::kani_proofs::EncodedTimePair::new(
        TIME_PROJECTION.first_ticks.load(Ordering::Relaxed),
        TIME_PROJECTION.second_ticks.load(Ordering::Relaxed),
        TIME_PROJECTION.first_seconds.load(Ordering::Relaxed),
        TIME_PROJECTION.second_seconds.load(Ordering::Relaxed),
    );
    pair.resolve(ticks)
        .expect("timestamp is outside the checked input pair")
}

#[kani::proof]
#[kani::unwind(1)]
fn projection_state_does_not_change_empty_vector_initialization() {
    let first: i64 = kani::any();
    let second: i64 = kani::any();
    prepare_time_pair(first, second, kani::any(), kani::any());
    let empty = Vec::<u32>::new();
    assert!(empty.is_empty());
    assert!(empty.capacity() == 0);
    kani::cover!(first == i64::MIN && second == i64::MAX);
    kani::cover!(first == second);
}

#[kani::proof]
#[kani::unwind(1)]
fn prepared_projection_resolves_both_registered_timestamps() {
    let first: i64 = kani::any();
    let second: i64 = kani::any();
    let first_seconds: u32 = kani::any();
    let second_seconds: u32 = kani::any();
    prepare_time_pair(first, second, first_seconds, second_seconds);
    assert!(projected_mtime(first) == first_seconds);
    assert!(
        projected_mtime(second)
            == if first == second {
                first_seconds
            } else {
                second_seconds
            }
    );
    kani::cover!(first == i64::MIN && second == i64::MAX);
    kani::cover!(first == second);
}

#[kani::proof]
#[kani::unwind(1)]
#[kani::solver(z3)]
fn timestamp_contract_preserves_clamping_in_the_runtime_build() {
    use fmf_core::dates::{FILETIME_UNIX_EPOCH, TICKS_PER_SECOND, mtime_ticks_to_secs};
    let ticks: i64 = kani::any();
    let seconds = mtime_ticks_to_secs(ticks);
    assert_eq!(seconds == 0, ticks < FILETIME_UNIX_EPOCH + TICKS_PER_SECOND);
    assert_eq!(
        seconds == u32::MAX,
        ticks >= FILETIME_UNIX_EPOCH + i64::from(u32::MAX) * TICKS_PER_SECOND
    );
    kani::cover!(ticks == i64::MIN);
    kani::cover!(ticks == i64::MAX);
}

fn check_link_refresh<const REPLACE: bool, const VALID_PARENT: bool>(
    aspect: MetadataAspect,
) -> (bool, u64) {
    let root = ROOT_FRN;
    let object = SHARED_FILE_FRN;
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
    let updated = RawEntry {
        size: kani::any(),
        mtime: kani::any(),
        is_reparse: kani::any(),
        is_hidden: kani::any(),
        is_system: kani::any(),
        ..original
    };
    prepare_time_pair(original.mtime, updated.mtime, kani::any(), kani::any());
    kani::cover!(original.mtime == i64::MIN && updated.mtime == i64::MAX);
    kani::cover!(original.mtime == updated.mtime);
    super::super::overflow_model::check_link_refresh::<REPLACE, VALID_PARENT>(
        &original, &updated, aspect,
    )
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn replacing_link_preserves_topology_and_metadata_generation() {
    let (accepted, size) = check_link_refresh::<true, true>(MetadataAspect::Topology);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn appending_link_preserves_topology_and_metadata_generation() {
    let (accepted, size) = check_link_refresh::<false, true>(MetadataAspect::Topology);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn invalid_parent_preserves_metadata_when_replacing() {
    let (accepted, size) = check_link_refresh::<true, false>(MetadataAspect::All);
    assert!(!accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn invalid_parent_preserves_metadata_when_appending() {
    let (accepted, size) = check_link_refresh::<false, false>(MetadataAspect::All);
    assert!(!accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn replacing_link_refreshes_every_shared_size() {
    let (accepted, size) = check_link_refresh::<true, true>(MetadataAspect::Sizes);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn appending_link_refreshes_every_shared_size() {
    let (accepted, size) = check_link_refresh::<false, true>(MetadataAspect::Sizes);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn replacing_link_refreshes_every_shared_timestamp() {
    let (accepted, size) = check_link_refresh::<true, true>(MetadataAspect::Timestamps);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn appending_link_refreshes_every_shared_timestamp() {
    let (accepted, size) = check_link_refresh::<false, true>(MetadataAspect::Timestamps);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn replacing_link_refreshes_every_shared_attribute() {
    let (accepted, size) = check_link_refresh::<true, true>(MetadataAspect::Attributes);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::solver(cadical)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn appending_link_refreshes_every_shared_attribute() {
    let (accepted, size) = check_link_refresh::<false, true>(MetadataAspect::Attributes);
    assert!(accepted);
    kani::cover!(size >= u64::from(u32::MAX));
    kani::cover!(size < u64::from(u32::MAX));
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
