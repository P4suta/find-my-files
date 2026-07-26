use super::*;
use crate::index::testutil::TestDir;
use crate::index::{Frn, RawEntry, SortKey, VolumeIndexBuilder};
use crate::query::QueryOptions;

fn vol(label: &str, names: &[(&str, u64)]) -> VolumeIndex {
    let mut b = VolumeIndexBuilder::new(label, 5);
    for (i, (name, size)) in names.iter().enumerate() {
        let units: Vec<u16> = name.encode_utf16().collect();
        b.push(RawEntry {
            parent_frn: Frn(5),
            frn: Frn((1 << 48) | (100 + i as u64)),
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

/// Engine on a fresh [`TestDir`] — the writer lock makes a shared dir a
/// cross-test collision under the default parallel test runner. Callers
/// hold the guard (`let (_dir, e) = …`) so it drops *after* the engine.
fn test_engine() -> (TestDir, Arc<Engine>) {
    let dir = TestDir::new();
    let e = Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("engine create");
    (dir, e)
}

fn engine_with_two_volumes() -> (TestDir, Arc<Engine>) {
    let (dir, e) = test_engine();
    e.insert_ready_volume("C:", vol("C:", &[("alpha.txt", 10), ("gamma.txt", 30)]));
    e.insert_ready_volume("D:", vol("D:", &[("beta.txt", 20), ("delta.txt", 40)]));
    (dir, e)
}

#[test]
fn query_merges_volumes_in_name_order() {
    let (_dir, e) = engine_with_two_volumes();
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
fn query_rejects_utf8_over_the_explicit_work_budget() {
    let (_dir, engine) = test_engine();
    let query = "あ".repeat(fmf_contract::limits::MAX_QUERY_BYTES as usize / "あ".len() + 1);
    assert!(matches!(
        engine.query(&query, &QueryOptions::default()),
        Err(EngineError::QueryTooLong {
            actual,
            maximum: fmf_contract::limits::MAX_QUERY_BYTES,
        }) if actual == query.len()
    ));
}

#[test]
fn fill_page_rejects_before_crossing_the_encoded_payload_budget() {
    let (_dir, engine) = engine_with_two_volumes();
    let result = engine.query("alpha", &QueryOptions::default()).unwrap().0;
    let (rows, blob) = result.fill_page(0, 1).unwrap();
    let encoded_len = 8 + rows.len() * fmf_contract::pod::FmfRow::LEN + blob.len();

    assert!(matches!(
        result.fill_page_with_limit(0, 1, encoded_len - 1),
        Err(EngineError::PageEncoding(
            "encoded page exceeds the maximum payload length"
        ))
    ));
    assert!(result.fill_page_with_limit(0, 1, encoded_len).is_ok());
}

#[test]
fn fill_page_preserves_a_valid_parent_path_longer_than_u16() {
    let mut builder = VolumeIndexBuilder::new("C:", 5);
    let component = vec![b'x' as u16; 255];
    let mut parent = 5;
    for record in 10..270 {
        builder.push(RawEntry {
            parent_frn: Frn(parent),
            frn: Frn((1 << 48) | record),
            name_utf16: &component,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        parent = record;
    }
    let leaf = "leaf.txt".encode_utf16().collect::<Vec<_>>();
    builder.push(RawEntry {
        parent_frn: Frn(parent),
        frn: Frn((1 << 48) | 0x03E8),
        name_utf16: &leaf,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 1,
        mtime: 0,
    });

    let (_dir, engine) = test_engine();
    engine.insert_ready_volume("C:", builder.finish());
    let result = engine.query("leaf", &QueryOptions::default()).unwrap().0;
    let (rows, blob) = result.fill_page(0, 1).unwrap();
    let row = rows[0];

    assert!(row.parent_path_len > u16::MAX as u32);
    assert_eq!(row._reserved, 0);
    let start = row.parent_path_off as usize;
    let end = start + row.parent_path_len as usize;
    assert_eq!(end, blob.len());
    assert!(blob[start..end].starts_with(b"C:\\"));
}

#[test]
fn index_start_skips_an_already_indexed_volume() {
    let (_dir, e) = test_engine();
    e.insert_ready_volume("C:", vol("C:", &[("alpha.txt", 10)]));
    assert_eq!(e.status().len(), 1);
    // A reconnecting client re-sends IndexStart for C: on every connect, and
    // the service also calls it at startup — it must be a no-op, not a second
    // slot. Duplicate slots make every query return C:'s rows once per copy
    // (the "each result appears N times" bug).
    e.start_canonical_volumes(&["C:".to_string()]);
    assert_eq!(
        e.status().len(),
        1,
        "index_start of an already-indexed volume must not add a duplicate slot"
    );
}

#[test]
fn volume_label_validation_is_exactly_letter_colon() {
    use super::volume::is_valid_volume_label;
    for good in ["C:", "c:", "D:", "z:", "A:"] {
        assert!(is_valid_volume_label(good), "{good:?} should be valid");
    }
    for bad in [
        "",                  // empty
        "C",                 // missing colon
        ":",                 // missing letter
        "1:",                // not a letter
        "CC:",               // two letters
        "C:\\",              // trailing separator
        "C:\\x",             // path under the volume
        "..\\evil",          // traversal
        "C:..\\..\\windows", // colon present but escapes
        "\\\\?\\C:",         // device path prefix
    ] {
        assert!(!is_valid_volume_label(bad), "{bad:?} should be rejected");
    }
}

/// Invalid `IndexStart` input is a synchronous, transactional rejection: it
/// creates no slots, worker threads, or misleading asynchronous events.
#[test]
fn index_start_rejects_malformed_volume_labels() {
    use std::sync::Mutex as StdMutex;

    let (_dir, e) = test_engine();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let captured = events.clone();
    e.set_event_sink(Some(Arc::new(move |event| {
        captured.lock().expect("event lock").push(event.clone());
    })));
    let error = e
        .index_start(&[
            "..\\evil".to_string(),
            "C:\\windows".to_string(),
            "C".to_string(),
            String::new(),
            "CC:".to_string(),
            "1:".to_string(),
        ])
        .expect_err("malformed request must be rejected synchronously");
    assert_eq!(
        error,
        super::IndexStartError::MalformedLabel { position: 0 }
    );
    assert_eq!(
        e.status().len(),
        0,
        "malformed labels must not create volume slots"
    );
    assert!(
        events.lock().expect("event lock").is_empty(),
        "request validation must not fabricate VolumeFailed events"
    );
}

#[test]
fn index_start_validation_is_canonical_unique_and_fixed_ntfs_only() {
    use super::IndexStartError;
    use super::volume::validate_index_start_volumes;

    let available = ["C:".to_string(), "D:".to_string()];
    assert_eq!(
        validate_index_start_volumes(&["c:".to_string(), "D:".to_string()], &available),
        Ok(vec!["C:".to_string(), "D:".to_string()])
    );
    assert_eq!(
        validate_index_start_volumes(&["C:".to_string(), "c:".to_string()], &available),
        Err(IndexStartError::DuplicateLabel {
            label: "C:".to_string()
        })
    );
    assert_eq!(
        validate_index_start_volumes(&["E:".to_string()], &available),
        Err(IndexStartError::UnavailableVolume {
            label: "E:".to_string()
        })
    );
}

#[test]
fn worker_spawn_failure_rolls_back_the_slot_and_reports_failure() {
    use std::sync::Mutex as StdMutex;

    let (dir, e) = test_engine();
    let events = Arc::new(StdMutex::new(Vec::new()));
    let captured = events.clone();
    e.set_event_sink(Some(Arc::new(move |event| {
        captured.lock().expect("event lock").push(event.clone());
    })));

    let store = Arc::new(super::seams::WinSnapshotStore::new(dir.join("c.fmfidx")));
    let slot = Arc::new(super::volume::VolumeSlot::scanning("C:".to_owned(), store));
    e.volumes.write().push(slot.clone());
    e.volume_spawn_failed(
        &slot,
        &std::io::Error::new(std::io::ErrorKind::WouldBlock, "scripted"),
    );

    assert!(
        e.status().is_empty(),
        "failed provisional slot must not suppress a later retry"
    );
    assert!(events.lock().expect("event lock").iter().any(|event| {
        matches!(
            event,
            EngineEvent::VolumeFailed { volume, message }
                if volume == "C:" && message.contains("scripted")
        )
    }));
}

#[test]
fn paging_is_a_slice_and_size_sort_descends() {
    let (_dir, e) = engine_with_two_volumes();
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
    let (_dir, e) = engine_with_two_volumes();
    let r = e.query("beta", &QueryOptions::default()).unwrap().0;
    let rows = r.page(0, 1).unwrap();
    assert_eq!(rows[0].parent_path, b"D:\\");
}

#[test]
fn rebuilt_volume_hard_stales_open_results() {
    let (_dir, e) = engine_with_two_volumes();
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
    let (_dir, e) = engine_with_two_volumes();
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

/// `unchanged` is exact identity against an explicit live presentation basis,
/// never the engine-global refinement cache (ADR-0044).
#[test]
fn idle_requery_of_identical_results_reports_unchanged() {
    let (_dir, e) = engine_with_two_volumes();
    let opt = QueryOptions::default();
    let (basis, t1) = e.query("txt", &opt).unwrap();
    assert!(!t1.unchanged, "first run has no previous result");

    // A no-op USN batch: generation bumps, ids stay identical.
    for slot in e.volumes.read().iter() {
        let mut g = slot.index.write();
        let idx = g.as_mut().unwrap();
        let n = idx.len() as u32;
        idx.merge_new_into_permutations(n);
    }
    let cancellation = QueryCancellation::new();
    let (same, t2) = e
        .query_cancellable("txt", &opt, &cancellation, Some(&basis))
        .unwrap();
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
            parent_frn: Frn(5),
            frn: Frn((1 << 48) | 0x3E7),
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
    let (r3, t3) = e
        .query_cancellable("txt", &opt, &cancellation, Some(&same))
        .unwrap();
    assert!(!t3.unchanged, "a new hit must repaint");
    assert_eq!(r3.len(), 5);

    // Query text is irrelevant to presentation identity: a different query
    // producing the same ordered IDs is safe to refresh in place.
    let (_, t4) = e
        .query_cancellable("tx", &opt, &cancellation, Some(&r3))
        .unwrap();
    assert!(t4.unchanged);
    let (_, t5) = e.query("tx", &opt).unwrap();
    assert!(!t5.unchanged, "no explicit basis means no identity claim");
}

#[test]
fn cancelled_query_publishes_no_refine_cache_or_served_metric() {
    let (_dir, e) = engine_with_two_volumes();
    let opt = QueryOptions::default();
    e.query("a", &opt).unwrap();
    let before_metrics = e.metrics_snapshot().recent_queries.len();
    let before_ids: Vec<_> = e
        .volumes
        .read()
        .iter()
        .map(|slot| slot.last_query.lock().as_ref().unwrap().ids.clone())
        .collect();

    let cancellation = QueryCancellation::new();
    cancellation.cancel();
    assert!(matches!(
        e.query_cancellable("al", &opt, &cancellation, None),
        Err(EngineError::Cancelled)
    ));

    assert_eq!(e.metrics_snapshot().recent_queries.len(), before_metrics);
    for (slot, before) in e.volumes.read().iter().zip(before_ids) {
        let cache = slot.last_query.lock();
        assert!(Arc::ptr_eq(&cache.as_ref().unwrap().ids, &before));
    }
}

#[test]
fn status_reports_ready_volumes() {
    let (_dir, e) = engine_with_two_volumes();
    let st = e.status();
    assert_eq!(st.len(), 2);
    assert!(
        st.iter()
            .all(|(_, p, n)| *p == VolumeState::Ready && *n > 0)
    );
}

/// Real-volume E2E: `index_start` → `VolumeReady` → query → snapshot save on
/// shutdown → `load_from` restores the same entry count. Run from an elevated
/// Run with `just test-admin` from an elevated terminal.
#[cfg(windows)]
#[test]
fn flush_saves_dirty_volumes_and_skips_clean_ones() {
    let (_dir, e) = test_engine();
    e.insert_ready_volume("C:", vol("C:", &[("alpha.txt", 10)]));
    // First flush writes the snapshot…
    assert_eq!(e.flush(), 1);
    // …and an unchanged volume is skipped (the periodic-timer common case).
    assert_eq!(e.flush(), 0);
    // A structural replacement (what a journal-gone rescan does) is dirty.
    e.replace_ready_volume("C:", vol("C:", &[("alpha.txt", 10), ("beta.txt", 20)]));
    assert_eq!(e.flush(), 1);
}

#[cfg(windows)]
#[test]
fn second_engine_on_same_index_dir_is_locked() {
    let dir = TestDir::new();
    let first = Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("first engine");
    match Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    }) {
        Err(EngineCreateError::Locked(pid)) => assert_eq!(pid, Some(std::process::id())),
        Err(e) => panic!("expected Locked, got {e}"),
        Ok(_) => panic!("expected Locked, got a second engine"),
    }
    drop(first);
    Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("lock must free on drop");
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
fn engine_e2e_scan_query_snapshot_restore() {
    use std::sync::mpsc;
    use std::time::Duration;

    if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
        eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
        return;
    }

    // Fresh per-run index dir → guaranteed full-scan path (no stale snapshot).
    let dir = TestDir::new();

    let e = Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("engine create");
    let (tx, rx) = mpsc::channel::<EngineEvent>();
    e.set_event_sink(Some(Arc::new(move |ev| {
        let _ = tx.send(ev.clone());
    })));
    e.index_start(&["C:".to_string()])
        .expect("C: must be an attached fixed NTFS volume");

    let ready_entries = loop {
        match rx.recv_timeout(Duration::from_mins(10)) {
            Ok(EngineEvent::VolumeReady { entries, .. }) => break entries,
            Ok(EngineEvent::VolumeFailed { message, .. }) => panic!("volume failed: {message}"),
            Ok(_) => {} // Progress / IndexChanged / EngineError
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
}
