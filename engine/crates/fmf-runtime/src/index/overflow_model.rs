use super::{EntryId, Frn};
use std::{borrow::Borrow, hash::Hash};

#[cfg(kani)]
const fn reject_storage_observation() {
    panic!("Storage observations are outside the keyed map model");
}

#[derive(Default)]
pub(super) struct OverflowMap {
    entries: [Option<(EntryId, u64)>; MAX_ENTRIES],
}

const MAX_ENTRIES: usize = 4;
const FIRST_LINK: EntryId = 1;
const SECOND_LINK: EntryId = 2;
const UNRELATED_LINK: EntryId = 3;
const APPENDED_LINK: EntryId = 4;
const INITIAL_ROWS: usize = APPENDED_LINK as usize;
const NAME_OFFSETS: [u32; INITIAL_ROWS] = [0, 2, 3, 4];
const NAME_IDS: [u32; INITIAL_ROWS] = [0, 1, 2, 3];
const UNRELATED_SIZE: u32 = 7;
const FIXTURE_SEQUENCE: u64 = 1u64 << (u64::BITS - u16::BITS);
pub(super) const ROOT_FRN: Frn = Frn(FIXTURE_SEQUENCE | crate::ondisk::ntfs::ROOT_RECORD);
pub(super) const SHARED_FILE_FRN: Frn = Frn(FIXTURE_SEQUENCE | 0x0064);
const UNRELATED_FRN: Frn = Frn(FIXTURE_SEQUENCE | 0x0065);
const MISSING_PARENT: Frn = Frn(FIXTURE_SEQUENCE | 0x03e7);

impl OverflowMap {
    pub(super) fn get<Q>(&self, key: &Q) -> Option<&u64>
    where
        EntryId: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        for slot in &self.entries {
            if let Some((entry, value)) = slot
                && <EntryId as Borrow<Q>>::borrow(entry) == key
            {
                return Some(value);
            }
        }
        None
    }

    pub(super) fn insert(&mut self, key: EntryId, value: u64) -> Option<u64> {
        for slot in &mut self.entries {
            if let Some((entry, previous)) = slot
                && *entry == key
            {
                return Some(std::mem::replace(previous, value));
            }
        }
        for slot in &mut self.entries {
            if slot.is_none() {
                *slot = Some((key, value));
                return None;
            }
        }
        panic!("overflow map model capacity exceeded");
    }

    pub(super) fn remove<Q>(&mut self, key: &Q) -> Option<u64>
    where
        EntryId: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        for slot in &mut self.entries {
            if slot
                .as_ref()
                .is_some_and(|(entry, _)| <EntryId as Borrow<Q>>::borrow(entry) == key)
            {
                return slot.take().map(|(_, value)| value);
            }
        }
        None
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&EntryId, &u64)> {
        #[cfg(kani)]
        reject_storage_observation();
        self.entries
            .iter()
            .flatten()
            .map(|(key, value)| (key, value))
    }

    pub(super) fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    pub(super) const fn capacity(&self) -> usize {
        #[cfg(kani)]
        reject_storage_observation();
        self.entries.len()
    }

    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "Fixed storage preserves the HashMap method signature."
    )]
    pub(super) const fn shrink_to_fit(&mut self) {
        #[cfg(kani)]
        reject_storage_observation();
    }
}

impl std::ops::Index<&EntryId> for OverflowMap {
    type Output = u64;

    fn index(&self, key: &EntryId) -> &Self::Output {
        self.get(key).expect("overflow size must exist")
    }
}

impl FromIterator<(EntryId, u64)> for OverflowMap {
    fn from_iter<T: IntoIterator<Item = (EntryId, u64)>>(entries: T) -> Self {
        #[cfg(kani)]
        reject_storage_observation();
        let mut result = Self::default();
        for (key, value) in entries {
            result.insert(key, value);
        }
        result
    }
}

#[cfg(kani)]
#[path = "overflow_model_proofs.rs"]
mod kani_proofs;

fn fixture_column<T: Copy>(values: &[T]) -> Vec<T> {
    values.to_vec()
}

pub(super) fn shared_metadata_fixture(original: &super::RawEntry<'_>) -> super::VolumeIndex {
    use super::{NO_PARENT, VolumeIndex, flags};

    let attributes = (u8::from(original.is_reparse) * flags::REPARSE)
        | (u8::from(original.is_hidden) * flags::HIDDEN)
        | (u8::from(original.is_system) * flags::SYSTEM)
        | (u8::from(original.is_hidden || original.is_system) * flags::EXCLUDED);
    let mtime = crate::query::dates::mtime_ticks_to_secs(original.mtime);
    let mut index = VolumeIndex {
        dict_pool: fixture_column(b"c:abx"),
        dict_off: fixture_column(&NAME_OFFSETS),
        name_id: fixture_column(&NAME_IDS),
        orig_pool: fixture_column(b"C:"),
        orig_off: fixture_column(&[0, u32::MAX, u32::MAX, u32::MAX]),
        parent: fixture_column(&[NO_PARENT, 0, 0, 0]),
        size_lo: fixture_column(&[0, 0, 0, UNRELATED_SIZE]),
        size_ovf: super::OverflowSizes::default(),
        mtime: fixture_column(&[0, mtime, mtime, 0]),
        frn: fixture_column(&[
            original.parent_frn.0,
            original.frn.0,
            original.frn.0,
            UNRELATED_FRN.0,
        ]),
        flag: fixture_column(&[flags::IS_DIR, attributes, attributes, 0]),
        frn_index: super::frn::FrnIndex::default(),
        perm_name: Vec::new(),
        content_generation: 0,
        structural_generation: 0,
        stat_generation: 0,
        dir_topology_generation: 0,
        exclusion_tree_dirty: false,
        tombstones: 0,
        dead_name_bytes: 0,
        dict_appends_since_dedup: APPENDED_LINK,
        derived_cache: parking_lot::Mutex::new(None),
    };
    index.set_size(FIRST_LINK, original.size);
    index.set_size(SECOND_LINK, original.size);
    index
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MetadataAspect {
    All,
    Topology,
    UntouchedTopology,
    Generation,
    Sizes,
    Timestamps,
    Attributes,
}

impl MetadataAspect {
    fn includes(self, aspect: Self) -> bool {
        self == Self::All || self == aspect
    }
}

pub(super) fn check_link_refresh<const REPLACE: bool, const VALID_PARENT: bool>(
    original: &super::RawEntry<'_>,
    metadata: &super::RawEntry<'_>,
    aspect: MetadataAspect,
) -> (bool, u64) {
    use super::{RawEntry, VolumeIndex, flags};

    let root = original.parent_frn;
    let object = original.frn;
    let mut index = shared_metadata_fixture(original);
    assert_eq!(index.len(), INITIAL_ROWS);
    assert_eq!(index.frn(FIRST_LINK).0, object.0);
    assert_eq!(index.frn(SECOND_LINK).0, object.0);
    assert_eq!(index.stat_generation, 0);
    let previous_generation = index.stat_generation;
    let previous_seconds = index.mtime[FIRST_LINK as usize];
    let previous_tombstones = index.tombstones;
    let name = [u16::from(if REPLACE { b'a' } else { b'c' })];
    let updated = RawEntry {
        parent_frn: if VALID_PARENT { root } else { MISSING_PARENT },
        name_utf16: &name,
        ..*metadata
    };
    let result = index.upsert_link_usn(&updated);
    assert_eq!(result.is_ok(), VALID_PARENT);
    if VALID_PARENT {
        let Ok(inserted) = result else {
            panic!("the admitted parent must accept the file link");
        };
        let expected_seconds = crate::query::dates::mtime_ticks_to_secs(updated.mtime);
        if aspect.includes(MetadataAspect::Topology) {
            assert_eq!(inserted, APPENDED_LINK);
            assert_eq!(index.len(), INITIAL_ROWS + 1);
            assert_ne!(index.is_live(FIRST_LINK), REPLACE);
            assert_eq!(index.tombstones, previous_tombstones + u32::from(REPLACE));
        }
        for id in [
            if REPLACE { SECOND_LINK } else { FIRST_LINK },
            SECOND_LINK,
            APPENDED_LINK,
        ] {
            if aspect.includes(MetadataAspect::Topology) {
                assert!(index.is_live(id));
                assert_eq!(index.frn(id).0, object.0);
            }
            if aspect.includes(MetadataAspect::Sizes) {
                assert_eq!(index.size(id), updated.size);
            }
            if aspect.includes(MetadataAspect::Timestamps) {
                assert_eq!(index.mtime[id as usize], expected_seconds);
                if aspect == MetadataAspect::All {
                    assert_eq!(
                        index.mtime(id),
                        crate::query::dates::mtime_secs_to_ticks(expected_seconds)
                    );
                }
            }
            if aspect.includes(MetadataAspect::Attributes) {
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
        }
        if aspect.includes(MetadataAspect::Generation)
            && (original.size != updated.size || previous_seconds != expected_seconds)
        {
            assert!(index.stat_generation > previous_generation);
        }
    } else {
        assert_eq!(index.len(), INITIAL_ROWS);
        assert_eq!(index.tombstones, previous_tombstones);
        assert_eq!(index.stat_generation, previous_generation);
        for id in [FIRST_LINK, SECOND_LINK] {
            assert!(index.is_live(id));
            assert_eq!(index.frn(id).0, object.0);
            assert_eq!(index.size(id), original.size);
            assert_eq!(index.mtime[id as usize], previous_seconds);
            assert_eq!(
                index.mtime(id),
                crate::query::dates::mtime_secs_to_ticks(previous_seconds)
            );
            assert_eq!(index.is_reparse(id), original.is_reparse);
            assert_eq!(
                index.flag[id as usize] & flags::HIDDEN != 0,
                original.is_hidden
            );
            assert_eq!(
                index.flag[id as usize] & flags::SYSTEM != 0,
                original.is_system
            );
            assert_eq!(
                index.is_excluded(id),
                original.is_hidden || original.is_system
            );
        }
    }
    if aspect.includes(MetadataAspect::UntouchedTopology) {
        assert!(index.is_live(UNRELATED_LINK));
        assert_eq!(index.frn(UNRELATED_LINK).0, UNRELATED_FRN.0);
        assert_eq!(index.parent(UNRELATED_LINK), VolumeIndex::ROOT);
        assert_eq!(index.name(UNRELATED_LINK), b"x");
        assert!(index.is_dir(VolumeIndex::ROOT));
        assert!(index.is_live(VolumeIndex::ROOT));
        assert_eq!(index.frn(VolumeIndex::ROOT).0, root.0);
    }
    if aspect.includes(MetadataAspect::Sizes) {
        assert_eq!(index.size(UNRELATED_LINK), u64::from(UNRELATED_SIZE));
    }
    if aspect.includes(MetadataAspect::Timestamps) {
        assert_eq!(index.mtime[UNRELATED_LINK as usize], 0);
        if aspect == MetadataAspect::All {
            assert_eq!(index.mtime(UNRELATED_LINK), 0);
        }
    }
    if aspect.includes(MetadataAspect::Attributes) {
        assert!(!index.is_reparse(UNRELATED_LINK));
        assert!(!index.is_excluded(UNRELATED_LINK));
    }
    (result.is_ok(), updated.size)
}

#[cfg(test)]
mod tests {
    use super::OverflowMap;
    use crate::index::{RawEntry, VolumeIndexBuilder};

    #[test]
    fn checked_refresh_covers_each_admitted_transition() {
        let root = super::ROOT_FRN;
        let original = RawEntry {
            parent_frn: root,
            frn: super::SHARED_FILE_FRN,
            name_utf16: &[u16::from(b'a')],
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 7,
            mtime: 0,
        };
        for size in [0, u64::from(u32::MAX) - 1, u64::from(u32::MAX), u64::MAX] {
            for mtime in [
                i64::MIN,
                0,
                crate::query::dates::FILETIME_UNIX_EPOCH,
                i64::MAX,
            ] {
                for before in 0..8 {
                    for after in 0..8 {
                        let original = RawEntry {
                            size,
                            mtime,
                            is_reparse: before & 1 != 0,
                            is_hidden: before & 2 != 0,
                            is_system: before & 4 != 0,
                            ..original
                        };
                        let updated = RawEntry {
                            size: u64::MAX - size,
                            mtime: mtime.wrapping_neg(),
                            is_reparse: after & 1 != 0,
                            is_hidden: after & 2 != 0,
                            is_system: after & 4 != 0,
                            ..original
                        };
                        super::check_link_refresh::<true, true>(
                            &original,
                            &updated,
                            super::MetadataAspect::All,
                        );
                        super::check_link_refresh::<false, true>(
                            &original,
                            &updated,
                            super::MetadataAspect::All,
                        );
                        super::check_link_refresh::<true, false>(
                            &original,
                            &updated,
                            super::MetadataAspect::All,
                        );
                        super::check_link_refresh::<false, false>(
                            &original,
                            &updated,
                            super::MetadataAspect::All,
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "overflow map model capacity exceeded")]
    fn exceeding_the_model_bound_is_rejected() {
        let mut map = OverflowMap::default();
        for key in 0..=super::MAX_ENTRIES as u32 {
            map.insert(key, u64::from(key));
        }
    }

    #[test]
    fn direct_proof_fixture_matches_admitted_link_events() {
        let root = super::ROOT_FRN;
        let object = super::SHARED_FILE_FRN;
        let other = super::UNRELATED_FRN;
        for size in [
            0,
            u64::from(u32::MAX) - 1,
            u64::from(u32::MAX),
            u64::from(u32::MAX) + 1,
            u64::MAX,
        ] {
            for mtime in [0, crate::query::dates::FILETIME_UNIX_EPOCH, i64::MAX] {
                for attributes in 0..8 {
                    let mut events = VolumeIndexBuilder::root_index("C:", root).unwrap();
                    let original = RawEntry {
                        parent_frn: root,
                        frn: object,
                        name_utf16: &[u16::from(b'a')],
                        is_dir: false,
                        is_reparse: attributes & 1 != 0,
                        is_hidden: attributes & 2 != 0,
                        is_system: attributes & 4 != 0,
                        size,
                        mtime,
                    };
                    let direct = super::shared_metadata_fixture(&original);
                    let second = RawEntry {
                        name_utf16: &[u16::from(b'b')],
                        ..original
                    };
                    let unrelated = RawEntry {
                        frn: other,
                        name_utf16: &[u16::from(b'x')],
                        size: 7,
                        mtime: 0,
                        is_reparse: false,
                        is_hidden: false,
                        is_system: false,
                        ..original
                    };
                    for entry in [original, second, unrelated] {
                        events.upsert_link_usn(&entry).unwrap();
                    }
                    assert_eq!(events.dict_pool, direct.dict_pool);
                    assert_eq!(events.dict_off, direct.dict_off);
                    assert_eq!(events.orig_pool, direct.orig_pool);
                    assert_eq!(events.orig_off, direct.orig_off);
                    assert_eq!(events.name_id, direct.name_id);
                    assert_eq!(events.perm_name, direct.perm_name);
                    assert_eq!(events.dead_name_bytes, direct.dead_name_bytes);
                    assert_eq!(
                        events.dict_appends_since_dedup,
                        direct.dict_appends_since_dedup
                    );
                    assert_eq!(events.len(), direct.len());
                    assert_eq!(events.stat_generation, direct.stat_generation);
                    assert_eq!(events.content_generation, direct.content_generation);
                    assert_eq!(events.structural_generation, direct.structural_generation);
                    assert_eq!(
                        events.dir_topology_generation,
                        direct.dir_topology_generation
                    );
                    assert_eq!(events.exclusion_tree_dirty, direct.exclusion_tree_dirty);
                    assert_eq!(events.tombstones, direct.tombstones);
                    for id in 0..events.len() as u32 {
                        assert_eq!(events.frn(id), direct.frn(id));
                        assert_eq!(events.parent(id), direct.parent(id));
                        assert_eq!(events.name(id), direct.name(id));
                        assert_eq!(events.flag[id as usize], direct.flag[id as usize]);
                        assert_eq!(events.size(id), direct.size(id));
                        assert_eq!(events.mtime(id), direct.mtime(id));
                    }
                }
            }
        }
    }

    #[test]
    fn operation_sequences_match_the_production_map_contract() {
        let keys = [1, 2, 4];
        let values = [0, u64::from(u32::MAX) - 1, u64::from(u32::MAX), u64::MAX];
        let actions = keys.len() * (values.len() + 1);
        for mut sequence in 0..actions.pow(4) {
            let mut model = OverflowMap::default();
            let mut actual = rustc_hash::FxHashMap::default();
            for _ in 0..4 {
                let action = sequence % actions;
                sequence /= actions;
                let key = keys[action % keys.len()];
                let operation = action / keys.len();
                if operation < values.len() {
                    let value = values[operation];
                    assert_eq!(model.insert(key, value), actual.insert(key, value));
                } else {
                    assert_eq!(model.remove(&key), actual.remove(&key));
                }
                assert_eq!(model.len(), actual.len());
                for key in keys {
                    assert_eq!(model.get(&key), actual.get(&key));
                }
                assert!(
                    model
                        .iter()
                        .all(|(key, value)| actual.get(key) == Some(value))
                );
            }
            model.shrink_to_fit();
            assert!(model.capacity() >= model.len());
        }
    }
}
