use super::{BatchStats, Frn, RawEntry, UsnRecord, apply_metadata_change};
use crate::index::VolumeIndexBuilder;

#[kani::proof]
#[kani::unwind(17)]
fn metadata_refresh_preserves_exact_rows_and_outcome_counts() {
    let root = Frn((1 << 48) | 5);
    let object = Frn((1 << 48) | 100);
    let unrelated = Frn((1 << 48) | 101);
    let mut index = VolumeIndexBuilder::root_index("C:", root).unwrap();
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
    index.upsert_link_usn(&original).unwrap();
    index
        .upsert_link_usn(&RawEntry {
            name_utf16: &[u16::from(b'b')],
            ..original
        })
        .unwrap();
    let untouched = index
        .upsert_link_usn(&RawEntry {
            frn: unrelated,
            name_utf16: &[u16::from(b'c')],
            ..original
        })
        .unwrap();

    let exists: bool = kani::any();
    let has_stat: bool = kani::any();
    let is_hidden: bool = kani::any();
    let is_system: bool = kani::any();
    let is_reparse: bool = kani::any();
    let size: u64 = kani::any();
    let mtime: i64 = kani::any();
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
    assert_eq!(
        stats,
        BatchStats {
            stat_updated: u32::from(exists && has_stat),
            stat_failures: u32::from(exists && !has_stat),
            ignored: u32::from(!exists || (!has_stat && !changed_attributes)),
            ..BatchStats::default()
        }
    );
    assert_eq!(index.len(), 4);
    assert_eq!(index.entries_by_frn(object).count(), 2);
    for id in index.entries_by_frn(object) {
        assert!(index.is_live(id));
        assert_eq!(index.parent(id), crate::index::VolumeIndex::ROOT);
        assert_eq!(index.size(id), if exists && has_stat { size } else { 7 });
        assert_eq!(
            index.mtime(id),
            if exists && has_stat {
                crate::query::dates::mtime_secs_to_ticks(crate::query::dates::mtime_ticks_to_secs(
                    mtime,
                ))
            } else {
                0
            }
        );
        assert_eq!(index.is_reparse(id), exists && is_reparse);
        assert_eq!(index.is_excluded(id), exists && (is_hidden || is_system));
    }
    assert_eq!(index.frn(untouched), unrelated);
    assert_eq!(index.size(untouched), 7);
    assert_eq!(index.mtime(untouched), 0);
    assert!(!index.is_reparse(untouched));
    assert!(!index.is_excluded(untouched));
    kani::cover!(!exists && has_stat);
    kani::cover!(exists && has_stat && size > u64::from(u32::MAX));
    kani::cover!(exists && !has_stat && changed_attributes);
    kani::cover!(exists && !has_stat && !changed_attributes);
}
