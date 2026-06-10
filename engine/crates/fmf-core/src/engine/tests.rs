use super::*;
use crate::index::{RawEntry, SortKey, VolumeIndexBuilder};
use crate::query::QueryOptions;

fn vol(label: &str, names: &[(&str, u64)]) -> VolumeIndex {
    let mut b = VolumeIndexBuilder::new(label, 5);
    for (i, (name, size)) in names.iter().enumerate() {
        let units: Vec<u16> = name.encode_utf16().collect();
        b.push(RawEntry {
            record: 100 + i as u64,
            parent_record: 5,
            frn: (1 << 48) | (100 + i as u64),
            name_utf16: &units,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: *size,
            mtime: i as i64,
        });
    }
    b.finish()
}

fn engine_with_two_volumes() -> Arc<Engine> {
    let e = Engine::new(EngineConfig {
        index_dir: std::env::temp_dir(),
    });
    e.insert_ready_volume("C:", vol("C:", &[("alpha.txt", 10), ("gamma.txt", 30)]));
    e.insert_ready_volume("D:", vol("D:", &[("beta.txt", 20), ("delta.txt", 40)]));
    e
}

#[test]
fn query_merges_volumes_in_name_order() {
    let e = engine_with_two_volumes();
    let r = e.query("txt", &QueryOptions::default()).unwrap().0;
    let rows = r.page(0, 10).unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["alpha.txt", "beta.txt", "delta.txt", "gamma.txt"]
    );
    // entry_ref carries the volume ordinal in the high half.
    assert_eq!(rows[0].entry_ref >> 32, 0);
    assert_eq!(rows[1].entry_ref >> 32, 1);
}

#[test]
fn paging_is_a_slice_and_size_sort_descends() {
    let e = engine_with_two_volumes();
    let opt = QueryOptions {
        sort: SortKey::Size,
        desc: true,

        ..Default::default()
    };
    let r = e.query("txt", &opt).unwrap().0;
    assert_eq!(r.len(), 4);
    let page = r.page(1, 2).unwrap();
    let sizes: Vec<u64> = page.iter().map(|r| r.size).collect();
    assert_eq!(sizes, vec![30, 20]);
    // Out-of-range page is empty, not an error.
    assert!(r.page(99, 5).unwrap().is_empty());
}

#[test]
fn parent_paths_come_back_per_volume() {
    let e = engine_with_two_volumes();
    let r = e.query("beta", &QueryOptions::default()).unwrap().0;
    let rows = r.page(0, 1).unwrap();
    assert_eq!(rows[0].parent_path, b"D:\\");
}

#[test]
fn rebuilt_volume_hard_stales_open_results() {
    let e = engine_with_two_volumes();
    let r = e.query("txt", &QueryOptions::default()).unwrap().0;
    assert_eq!(r.page(0, 10).unwrap().len(), 4);

    // Journal gone → full rescan: C:'s index is rebuilt from scratch and
    // swapped into the slot. The open ResultSet still holds C: entry ids
    // from the old index — without a structural bump it would silently
    // serve rows for unrelated entries (docs/ARCHITECTURE.md: full rescan
    // hard-stales open handles).
    e.replace_ready_volume("C:", vol("C:", &[("omega.txt", 1), ("zeta.txt", 2)]));

    assert!(matches!(r.page(0, 10), Err(EngineError::Stale)));
}

#[test]
fn status_reports_ready_volumes() {
    let e = engine_with_two_volumes();
    let st = e.status();
    assert_eq!(st.len(), 2);
    assert!(
        st.iter()
            .all(|(_, p, n)| *p == VolumePhase::Ready && *n > 0)
    );
}
