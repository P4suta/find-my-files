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
fn typing_refines_cached_results_and_invalidation_goes_cold() {
    let e = engine_with_two_volumes();
    let opt = QueryOptions::default();

    // Cold first query, refined on each extension, identical results.
    let (_, t1) = e.query("a", &opt).unwrap();
    assert_eq!(t1.cache, "miss");
    let (r2, t2) = e.query("al", &opt).unwrap();
    assert_eq!(t2.cache, "refine");
    let names: Vec<String> = r2
        .page(0, 10)
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
        .collect();
    assert_eq!(names, vec!["alpha.txt"]);

    // Widening goes cold but stays correct.
    let (r3, t3) = e.query("a", &opt).unwrap();
    assert_eq!(t3.cache, "miss");
    assert_eq!(r3.len(), 4); // alpha/gamma/beta/delta all contain "a"

    // Structural replacement invalidates the cache (and clears it).
    e.replace_ready_volume("C:", vol("C:", &[("omega.txt", 1)]));
    let (_, t4) = e.query("a t", &opt).unwrap();
    assert_eq!(t4.cache, "partial", "D: refines, rebuilt C: goes cold");
    let (_, t5) = e.query("a tx", &opt).unwrap();
    assert_eq!(t5.cache, "refine");
}

/// Idle USN traffic (logs, telemetry) re-queries the same text every few
/// hundred ms. When the id lists come back identical the trace must say so —
/// that flag is what stops the UI from repainting an unchanged screen.
#[test]
fn idle_requery_of_identical_results_reports_unchanged() {
    let e = engine_with_two_volumes();
    let opt = QueryOptions::default();
    let (_, t1) = e.query("txt", &opt).unwrap();
    assert!(!t1.unchanged, "first run has no previous result");

    // A no-op USN batch: generation bumps, ids stay identical.
    for slot in e.volumes.read().iter() {
        let mut g = slot.index.write();
        let idx = g.as_mut().unwrap();
        let n = idx.len() as u32;
        idx.merge_new_into_permutations(n);
    }
    let (_, t2) = e.query("txt", &opt).unwrap();
    assert!(t2.unchanged, "same query, same ids");
    assert_eq!(
        t2.cache, "miss",
        "the generation moved, so the cache was cold"
    );

    // A real change to the result set flips it off.
    {
        let volumes = e.volumes.read();
        let slot = volumes.iter().find(|s| s.label == "C:").unwrap();
        let mut g = slot.index.write();
        let idx = g.as_mut().unwrap();
        let first_new = idx.len() as u32;
        let units: Vec<u16> = "epsilon.txt".encode_utf16().collect();
        idx.upsert(&RawEntry {
            record: 999,
            parent_record: 5,
            frn: (1 << 48) | 999,
            name_utf16: &units,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 5,
            mtime: 5,
        });
        idx.merge_new_into_permutations(first_new);
    }
    let (r3, t3) = e.query("txt", &opt).unwrap();
    assert!(!t3.unchanged, "a new hit must repaint");
    assert_eq!(r3.len(), 5);

    // Different text is never "unchanged", but its stable repeat is.
    let (_, t4) = e.query("tx", &opt).unwrap();
    assert!(!t4.unchanged);
    let (_, t5) = e.query("tx", &opt).unwrap();
    assert!(t5.unchanged, "stable repeat via the refine path");
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

/// Real-volume E2E: index_start → VolumeReady → query → snapshot save on
/// shutdown → load_from restores the same entry count. Run from an elevated
/// shell: FMF_ADMIN_TESTS=1 cargo test -p fmf-core -- --ignored engine_e2e
#[test]
#[ignore]
fn engine_e2e_scan_query_snapshot_restore() {
    if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
        eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
        return;
    }
    use std::sync::mpsc;
    use std::time::Duration;

    // Fresh per-run index dir → guaranteed full-scan path (no stale snapshot).
    let dir = std::env::temp_dir().join(format!("fmf-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let e = Engine::new(EngineConfig {
        index_dir: dir.clone(),
    });
    let (tx, rx) = mpsc::channel::<EngineEvent>();
    e.set_event_sink(Some(Arc::new(move |ev| {
        let _ = tx.send(ev.clone());
    })));
    e.index_start(&["C:".to_string()]);

    let ready_entries = loop {
        match rx.recv_timeout(Duration::from_secs(600)) {
            Ok(EngineEvent::VolumeReady { entries, .. }) => break entries,
            Ok(EngineEvent::VolumeFailed { message, .. }) => panic!("volume failed: {message}"),
            Ok(_) => continue, // Progress / IndexChanged / EngineError
            Err(err) => panic!("no VolumeReady within timeout: {err}"),
        }
    };
    assert!(
        ready_entries > 10_000,
        "suspiciously small C: index: {ready_entries}"
    );

    let (r, _trace) = e
        .query("windows", &QueryOptions::default())
        .expect("query against the live index");
    assert!(!r.is_empty(), "'windows' must match something on C:");
    let rows = r.page(0, 10).unwrap();
    assert!(!rows.is_empty());
    assert!(
        rows.iter().all(|row| row.parent_path.starts_with(br"C:\")),
        "parent paths must resolve to the scanned volume"
    );

    // The tailing thread sits in a blocking journal read; generate volume
    // activity until shutdown's join completes so the test never hangs on an
    // otherwise idle machine (temp_dir lives on C: on a stock setup).
    let stop_tickle = Arc::new(AtomicBool::new(false));
    let tickle_flag = stop_tickle.clone();
    let tickle = std::thread::spawn(move || {
        let p = std::env::temp_dir().join("fmf-e2e-tickle.tmp");
        while !tickle_flag.load(Ordering::Relaxed) {
            let _ = std::fs::write(&p, b"tick");
            let _ = std::fs::remove_file(&p);
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    e.shutdown(); // joins the volume thread → snapshot saved with checkpoint
    stop_tickle.store(true, Ordering::Relaxed);
    tickle.join().unwrap();

    // After join the in-memory state is frozen; the saved snapshot must
    // restore to exactly the entry count the engine last reported.
    let final_entries = e
        .status()
        .iter()
        .find(|(v, _, _)| v == "C:")
        .map(|(_, _, n)| *n)
        .expect("C: slot still registered");
    let snapshot = dir.join("c.fmfidx");
    let (restored, journal_id, next_usn) =
        VolumeIndex::load_from(&snapshot).expect("snapshot written on shutdown and loadable");
    assert_ne!(journal_id, 0, "checkpoint must carry the journal id");
    assert!(next_usn > 0, "checkpoint must carry a USN cursor");
    assert_eq!(restored.live_len() as u64, final_entries);

    let _ = std::fs::remove_dir_all(&dir);
}
