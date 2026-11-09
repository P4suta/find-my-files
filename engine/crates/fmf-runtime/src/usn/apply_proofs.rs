use super::{BatchStats, Frn, RawEntry, UsnRecord, apply_metadata_change};
use crate::index::{
    prepare_time_pair, projected_mtime, shared_metadata_fixture, stored_mtime_seconds,
};

enum MetadataProperty {
    Outcomes,
    Topology,
    Sizes,
    Timestamps,
    Attributes,
}

struct MetadataRefreshCheck {
    holds: bool,
    exists: bool,
    has_stat: bool,
    changed_attributes: bool,
    size: u64,
    mtime: i64,
}

fn check_metadata_refresh(property: MetadataProperty) -> MetadataRefreshCheck {
    let mtime: i64 = kani::any();
    prepare_time_pair(0, mtime, 0, kani::any());
    let root = Frn((1 << 48) | 5);
    let object = Frn((1 << 48) | 100);
    let unrelated = Frn((1 << 48) | 101);
    let original = RawEntry {
        parent_frn: root,
        frn: object,
        name_utf16: &[u16::from(b'a')],
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 7,
        mtime: 0,
    };
    let mut index = shared_metadata_fixture(&original);
    let untouched = index.entries_by_frn(unrelated).next().unwrap();

    let exists: bool = kani::any();
    let has_stat: bool = kani::any();
    let is_hidden: bool = kani::any();
    let is_system: bool = kani::any();
    let is_reparse: bool = kani::any();
    let size: u64 = kani::any();
    let key = if exists { object } else { Frn((1 << 48) | 999) };
    let record = UsnRecord {
        usn: 0,
        frn: key.0,
        parent_frn: root.0,
        reason: 0,
        attributes: if is_hidden { 0x2 } else { 0 }
            | if is_system { 0x4 } else { 0 }
            | if is_reparse { 0x400 } else { 0 },
        name: vec![u16::from(b'a')],
    };
    let mut stats = BatchStats::default();
    apply_metadata_change(
        &mut index,
        key,
        &record,
        has_stat.then_some((size, mtime)),
        &mut stats,
    );
    let changed_attributes = is_hidden || is_system || is_reparse;
    let holds = match property {
        MetadataProperty::Outcomes => {
            stats
                == BatchStats {
                    stat_updated: u32::from(exists && has_stat),
                    stat_failures: u32::from(exists && !has_stat),
                    ignored: u32::from(!exists || (!has_stat && !changed_attributes)),
                    ..BatchStats::default()
                }
        }
        MetadataProperty::Topology => {
            index.len() == 4
                && index.entries_by_frn(object).count() == 2
                && index.entries_by_frn(object).all(|id| {
                    index.is_live(id) && index.parent(id) == crate::index::VolumeIndex::ROOT
                })
                && index.frn(untouched) == unrelated
        }
        MetadataProperty::Sizes => {
            index
                .entries_by_frn(object)
                .all(|id| index.size(id) == if exists && has_stat { size } else { 7 })
                && index.size(untouched) == 7
        }
        MetadataProperty::Timestamps => {
            index.entries_by_frn(object).all(|id| {
                stored_mtime_seconds(&index, id)
                    == if exists && has_stat {
                        crate::query::dates::mtime_ticks_to_secs(mtime)
                    } else {
                        0
                    }
            }) && stored_mtime_seconds(&index, untouched) == 0
        }
        MetadataProperty::Attributes => {
            index.entries_by_frn(object).all(|id| {
                index.is_reparse(id) == (exists && is_reparse)
                    && index.is_excluded(id) == (exists && (is_hidden || is_system))
            }) && !index.is_reparse(untouched)
                && !index.is_excluded(untouched)
        }
    };
    MetadataRefreshCheck {
        holds,
        exists,
        has_stat,
        changed_attributes,
        size,
        mtime,
    }
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn metadata_refresh_counts_each_outcome() {
    let check = check_metadata_refresh(MetadataProperty::Outcomes);
    assert!(check.holds);
    kani::cover!(!check.exists && check.has_stat);
    kani::cover!(check.exists && check.has_stat && check.size > u64::from(u32::MAX));
    kani::cover!(check.exists && !check.has_stat && check.changed_attributes);
    kani::cover!(check.exists && !check.has_stat && !check.changed_attributes);
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn metadata_refresh_preserves_link_topology() {
    let check = check_metadata_refresh(MetadataProperty::Topology);
    assert!(check.holds);
    kani::cover!(!check.exists && check.has_stat);
    kani::cover!(check.exists && check.has_stat);
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn metadata_refresh_updates_every_matching_size() {
    let check = check_metadata_refresh(MetadataProperty::Sizes);
    assert!(check.holds);
    kani::cover!(check.exists && check.has_stat && check.size > u64::from(u32::MAX));
    kani::cover!(check.exists && !check.has_stat);
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn metadata_refresh_updates_every_matching_timestamp() {
    let check = check_metadata_refresh(MetadataProperty::Timestamps);
    assert!(check.holds);
    kani::cover!(check.exists && check.has_stat && check.mtime == i64::MIN);
    kani::cover!(check.exists && check.has_stat && check.mtime == i64::MAX);
}

#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(fmf_core::dates::mtime_ticks_to_secs, projected_mtime)]
fn metadata_refresh_updates_only_matching_attributes() {
    let check = check_metadata_refresh(MetadataProperty::Attributes);
    assert!(check.holds);
    kani::cover!(check.exists && check.changed_attributes);
    kani::cover!(!check.exists && check.changed_attributes);
}
