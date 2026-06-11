//! Deterministic, unprivileged replays of the volume worker's failure
//! paths (ADR-0018, S4b). Scripted fakes stand in for the two OS seams
//! (`SnapshotStore`, `JournalSource`); events come through the real
//! `set_event_sink`, counters through the real per-engine `MetricsHub` —
//! only the OS edge is faked. The elevated FMF_ADMIN_TESTS suite remains
//! the second defense layer on real volumes and is untouched.
//!
//! Not covered here (by design — the 2-trait cap excludes the $MFT scan
//! from the seams): *completing* a full scan. The corrupt-snapshot test
//! drives the worker into the scan attempt and pins its failure surface;
//! the rescan test re-establishes via the restore path, which converges on
//! the same install → checkpoint → Ready tail the scan path uses.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::index::testutil::TestDir;
use crate::index::{RawEntry, VolumeIndex, VolumeIndexBuilder};
use crate::query::QueryOptions;
use crate::usn::records::reason;
use crate::usn::{JournalGone, ReadOutcome, StatFetcher, UsnError, UsnRecord};

use super::seams::{JournalSource, JournalView, SnapshotStore};
use super::worker::{
    CompactionVerdict, FullScanReason, SnapshotDecision, TailStep, compact_recheck,
    journal_gone_action, snapshot_decision,
};
use super::{Engine, EngineConfig, EngineEvent, VolumeState};

// ── Pure decision tables ────────────────────────────────────────────────

fn io_err(kind: std::io::ErrorKind) -> std::io::Error {
    std::io::Error::new(kind, "scripted")
}

#[test]
fn snapshot_decision_table() {
    use std::io::ErrorKind;
    let view = JournalView {
        journal_id: 7,
        first_usn: 100,
    };

    // Missing snapshot is the normal first run, not a failure.
    assert_eq!(
        snapshot_decision(Err(&io_err(ErrorKind::NotFound)), None),
        SnapshotDecision::FullScan(FullScanReason::FirstRun)
    );
    // Any other load error is a corrupt/unreadable snapshot.
    assert_eq!(
        snapshot_decision(Err(&io_err(ErrorKind::InvalidData)), None),
        SnapshotDecision::FullScan(FullScanReason::SnapshotUnusable)
    );
    assert_eq!(
        snapshot_decision(Err(&io_err(ErrorKind::PermissionDenied)), None),
        SnapshotDecision::FullScan(FullScanReason::SnapshotUnusable)
    );
    // Loaded but the journal couldn't be queried: don't trust the cursor.
    assert_eq!(
        snapshot_decision(Ok((7, 100)), None),
        SnapshotDecision::FullScan(FullScanReason::JournalQueryFailed)
    );
    // Journal id changed under the snapshot.
    assert_eq!(
        snapshot_decision(Ok((6, 100)), Some(view)),
        SnapshotDecision::FullScan(FullScanReason::CheckpointStale)
    );
    // Cursor already purged from the journal (next < first).
    assert_eq!(
        snapshot_decision(Ok((7, 99)), Some(view)),
        SnapshotDecision::FullScan(FullScanReason::CheckpointStale)
    );
    // Boundary: a cursor exactly at the oldest retained USN replays fine.
    assert_eq!(
        snapshot_decision(Ok((7, 100)), Some(view)),
        SnapshotDecision::Restore
    );
    assert_eq!(
        snapshot_decision(Ok((7, 101)), Some(view)),
        SnapshotDecision::Restore
    );
}

#[test]
fn journal_gone_action_maps_every_outcome() {
    let rec = usn_create(200, "x.txt");
    match journal_gone_action(Ok(ReadOutcome::Records {
        records: vec![rec.clone()],
        truncated: true,
    })) {
        TailStep::Apply { records, truncated } => {
            assert_eq!(records, vec![rec]);
            assert!(truncated, "the malformed-tail flag must survive");
        }
        _ => panic!("records must map to Apply"),
    }
    for gone in [
        JournalGone::EntryDeleted,
        JournalGone::DeleteInProgress,
        JournalGone::NotActive,
        JournalGone::IdMismatch,
    ] {
        match journal_gone_action(Ok(ReadOutcome::Gone(gone))) {
            TailStep::Rescan(g) => assert_eq!(g, gone),
            _ => panic!("every JournalGone variant must map to Rescan"),
        }
    }
    assert!(matches!(
        journal_gone_action(Err(UsnError::Fsctl(5))),
        TailStep::Fail(_)
    ));
}

#[test]
fn compact_recheck_table() {
    assert_eq!(compact_recheck(42, Some(42)), CompactionVerdict::Install);
    assert_eq!(compact_recheck(42, Some(43)), CompactionVerdict::Abort);
    assert_eq!(compact_recheck(42, None), CompactionVerdict::Abort);
}

// ── Scripted fakes for the two seams ────────────────────────────────────

enum LoadScript {
    /// A loadable snapshot with its persisted (journal_id, next_usn).
    /// Boxed: a `VolumeIndex` dwarfs the other variant (clippy).
    Found(Box<VolumeIndex>, u64, i64),
    /// Corrupt/unreadable file (anything but NotFound).
    Corrupt,
}

struct FakeStore {
    loads: Mutex<VecDeque<LoadScript>>,
    fail_saves: bool,
    save_attempts: AtomicU64,
    /// (journal_id, next_usn) of every *successful* save.
    saved: Mutex<Vec<(u64, i64)>>,
    removed: AtomicU64,
}

impl FakeStore {
    fn new(loads: Vec<LoadScript>, fail_saves: bool) -> Arc<Self> {
        Arc::new(Self {
            loads: Mutex::new(loads.into()),
            fail_saves,
            save_attempts: AtomicU64::new(0),
            saved: Mutex::new(Vec::new()),
            removed: AtomicU64::new(0),
        })
    }
}

impl SnapshotStore for FakeStore {
    fn load(&self) -> std::io::Result<(VolumeIndex, u64, i64)> {
        match self.loads.lock().pop_front() {
            Some(LoadScript::Found(idx, journal_id, next_usn)) => Ok((*idx, journal_id, next_usn)),
            Some(LoadScript::Corrupt) => Err(io_err(std::io::ErrorKind::InvalidData)),
            // Script exhausted: behave like a missing file (first run).
            None => Err(io_err(std::io::ErrorKind::NotFound)),
        }
    }

    fn file_bytes(&self) -> u64 {
        0
    }

    fn save_atomic(
        &self,
        _idx: &VolumeIndex,
        journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        self.save_attempts.fetch_add(1, Ordering::Relaxed);
        if self.fail_saves {
            return Err(io_err(std::io::ErrorKind::PermissionDenied));
        }
        self.saved.lock().push((journal_id, next_usn));
        Ok(())
    }

    fn remove(&self) {
        self.removed.fetch_add(1, Ordering::Relaxed);
    }
}

enum FakeRead {
    Batch(Vec<UsnRecord>),
    Gone(JournalGone),
    /// Park (returning benign empty wakeups) until the test opens the
    /// gate — lets a test act between two scripted reads without racing
    /// the worker.
    Gate(Arc<AtomicBool>),
}

/// One scripted journal lifetime (between `open` calls).
struct Incarnation {
    journal_id: u64,
    /// Cursor position at open ("current end of the journal").
    next_usn: i64,
    /// `query()`'s answer; `None` → Err (FSCTL failure).
    view: Option<JournalView>,
    reads: Vec<FakeRead>,
}

struct FakeJournal {
    opens: VecDeque<Incarnation>,
    journal_id: u64,
    next_usn: i64,
    view: Option<JournalView>,
    reads: VecDeque<FakeRead>,
    /// All scripted stat lookups fail (the storm) when false.
    stat_ok: bool,
    query_calls: Arc<AtomicU64>,
}

impl FakeJournal {
    fn new(opens: Vec<Incarnation>, stat_ok: bool, query_calls: Arc<AtomicU64>) -> Box<Self> {
        Box::new(Self {
            opens: opens.into(),
            journal_id: 0,
            next_usn: 0,
            view: None,
            reads: VecDeque::new(),
            stat_ok,
            query_calls,
        })
    }
}

impl JournalSource for FakeJournal {
    fn open(&mut self) -> Result<(), UsnError> {
        let inc = self.opens.pop_front().ok_or(UsnError::Fsctl(0))?;
        self.journal_id = inc.journal_id;
        self.next_usn = inc.next_usn;
        self.view = inc.view;
        self.reads = inc.reads.into();
        Ok(())
    }

    fn query(&mut self) -> Result<JournalView, UsnError> {
        self.query_calls.fetch_add(1, Ordering::Relaxed);
        self.view.ok_or(UsnError::Fsctl(0))
    }

    fn read_blocking(&mut self, _buf: &mut Vec<u8>) -> Result<ReadOutcome, UsnError> {
        while let Some(FakeRead::Gate(gate)) = self.reads.front() {
            if gate.load(Ordering::Relaxed) {
                self.reads.pop_front();
                continue;
            }
            std::thread::sleep(Duration::from_millis(2));
            return Ok(ReadOutcome::Records {
                records: Vec::new(),
                truncated: false,
            });
        }
        match self.reads.pop_front() {
            Some(FakeRead::Batch(records)) => {
                self.next_usn += records.len() as i64;
                Ok(ReadOutcome::Records {
                    records,
                    truncated: false,
                })
            }
            Some(FakeRead::Gone(gone)) => Ok(ReadOutcome::Gone(gone)),
            // The loop above consumed any leading gates.
            Some(FakeRead::Gate(_)) => unreachable!("gate handled before pop"),
            // Idle: a benign wakeup (empty batch) lets the worker re-check
            // its stop flag — the fake's stand-in for "the blocked read
            // returns on the next volume write".
            None => {
                std::thread::sleep(Duration::from_millis(2));
                Ok(ReadOutcome::Records {
                    records: Vec::new(),
                    truncated: false,
                })
            }
        }
    }

    fn journal_id(&self) -> u64 {
        self.journal_id
    }

    fn next_usn(&self) -> i64 {
        self.next_usn
    }

    fn set_next_usn(&mut self, usn: i64) {
        self.next_usn = usn;
    }

    fn open_stat_fetcher(&self) -> Result<Box<dyn StatFetcher>, UsnError> {
        Ok(Box::new(FakeStat { ok: self.stat_ok }))
    }
}

struct FakeStat {
    ok: bool,
}

impl StatFetcher for FakeStat {
    fn stat(&self, _frn: u64) -> Option<(u64, i64)> {
        self.ok.then_some((42, 9))
    }
}

// ── Harness helpers ─────────────────────────────────────────────────────

const WAIT: Duration = Duration::from_secs(10);

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

fn sink_channel(e: &Arc<Engine>) -> mpsc::Receiver<EngineEvent> {
    let (tx, rx) = mpsc::channel::<EngineEvent>();
    e.set_event_sink(Some(Arc::new(move |ev| {
        let _ = tx.send(ev.clone());
    })));
    rx
}

fn vol(label: &str, names: &[&str]) -> VolumeIndex {
    let mut b = VolumeIndexBuilder::new(label, 5);
    for (i, name) in names.iter().enumerate() {
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
            size: 10,
            mtime: i as i64,
        });
    }
    b.finish()
}

fn usn_create(frn_low: u64, name: &str) -> UsnRecord {
    UsnRecord {
        usn: 0,
        frn: (1 << 48) | frn_low,
        parent_frn: 5, // the builder's root record
        reason: reason::FILE_CREATE | reason::CLOSE,
        attributes: 0x20,
        name: name.encode_utf16().collect(),
    }
}

/// Lifecycle events of one volume, in arrival order, until `until` matches
/// (inclusive). EngineError (global diag forwarding — other tests' WARNs
/// land here too) and Progress are excluded from ordering assertions.
fn lifecycle_until(
    rx: &mpsc::Receiver<EngineEvent>,
    volume: &str,
    until: impl Fn(&EngineEvent) -> bool,
) -> Vec<EngineEvent> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("timeout; lifecycle so far: {seen:?}"));
        let ev = rx
            .recv_timeout(remaining)
            .unwrap_or_else(|e| panic!("no event within timeout ({e}); so far: {seen:?}"));
        let interesting = match &ev {
            EngineEvent::VolumeReady { volume: v, .. }
            | EngineEvent::IndexChanged { volume: v }
            | EngineEvent::RescanStarted { volume: v }
            | EngineEvent::VolumeFailed { volume: v, .. } => v == volume,
            EngineEvent::Progress { .. } | EngineEvent::EngineError { .. } => false,
        };
        if !interesting {
            continue;
        }
        let done = until(&ev);
        seen.push(ev);
        if done {
            return seen;
        }
    }
}

fn wait_counter(counter: &AtomicU64, target: u64, what: &str) {
    let deadline = Instant::now() + WAIT;
    while counter.load(Ordering::Relaxed) < target {
        assert!(
            Instant::now() < deadline,
            "{what} never reached {target} (now {})",
            counter.load(Ordering::Relaxed)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn phase_of(e: &Engine, volume: &str) -> VolumeState {
    e.status()
        .iter()
        .find(|(v, _, _)| v == volume)
        .map(|(_, p, _)| *p)
        .expect("volume registered")
}

// ── Failure-path replays ────────────────────────────────────────────────

/// Corrupt snapshot → counted + warned, then degrade to the full-scan
/// path. The scan itself is not behind a seam (2-trait cap), so the label
/// names a volume that cannot exist: the scan attempt fails identically
/// with or without elevation, proving the routing without touching real
/// volumes. The journal must never have been consulted — a failed load
/// spends no FSCTL.
#[test]
fn corrupt_snapshot_counts_and_degrades_to_full_scan() {
    let label = "FMFWK1:";
    let (_dir, e) = test_engine();
    let rx = sink_channel(&e);
    let store = FakeStore::new(vec![LoadScript::Corrupt], false);
    let query_calls = Arc::new(AtomicU64::new(0));
    let journal = FakeJournal::new(
        vec![Incarnation {
            journal_id: 7,
            next_usn: 100,
            view: Some(JournalView {
                journal_id: 7,
                first_usn: 1,
            }),
            reads: vec![],
        }],
        true,
        query_calls.clone(),
    );
    e.spawn_worker_with_seams(label, store.clone(), journal);

    let events = lifecycle_until(&rx, label, |ev| {
        matches!(ev, EngineEvent::VolumeFailed { .. })
    });
    // Degradation order: no Ready, no Rescan — straight to the (failing)
    // scan attempt.
    assert_eq!(events.len(), 1, "only VolumeFailed expected: {events:?}");
    assert_eq!(
        e.metrics()
            .counters
            .snapshot_load_failures
            .load(Ordering::Relaxed),
        1,
        "the corrupt load must be counted exactly once"
    );
    assert_eq!(
        query_calls.load(Ordering::Relaxed),
        0,
        "a failed load must not spend an FSCTL on the journal"
    );
    assert_eq!(phase_of(&e, label), VolumeState::Failed);
    e.shutdown();
}

/// Journal-gone while tailing → RescanStarted → re-establish → Ready. The
/// re-establish completes via the restore path (scan execution is outside
/// the seams), which converges on the same install → checkpoint → Ready
/// tail; the phase walk, the event order, the checkpoint invalidation, the
/// snapshot removal and the structural hard-stale are all the real thing.
#[test]
fn journal_gone_rescans_and_returns_to_ready() {
    let label = "FMFWK2:";
    let (_dir, e) = test_engine();
    let rx = sink_channel(&e);
    let store = FakeStore::new(
        vec![
            LoadScript::Found(Box::new(vol(label, &["alpha.txt"])), 7, 50),
            LoadScript::Found(Box::new(vol(label, &["beta.txt", "gamma.txt"])), 8, 400),
        ],
        false,
    );
    // The gate holds the journal alive until the test has opened a result
    // handle on the first index (the hard-stale probe below).
    let gate = Arc::new(AtomicBool::new(false));
    let journal = FakeJournal::new(
        vec![
            Incarnation {
                journal_id: 7,
                next_usn: 100,
                view: Some(JournalView {
                    journal_id: 7,
                    first_usn: 10,
                }),
                reads: vec![
                    FakeRead::Gate(gate.clone()),
                    FakeRead::Gone(JournalGone::EntryDeleted),
                ],
            },
            Incarnation {
                journal_id: 8,
                next_usn: 600,
                view: Some(JournalView {
                    journal_id: 8,
                    first_usn: 400,
                }),
                reads: vec![],
            },
        ],
        true,
        Arc::new(AtomicU64::new(0)),
    );
    e.spawn_worker_with_seams(label, store.clone(), journal);

    // A result handle opened on the first incarnation must go hard-stale
    // after the rescan installs the rebuilt index.
    let first_ready = lifecycle_until(&rx, label, |ev| {
        matches!(ev, EngineEvent::VolumeReady { .. })
    });
    // Entry counts include the builder's root entry: 1 root + alpha.txt.
    assert!(matches!(
        first_ready.as_slice(),
        [EngineEvent::VolumeReady { entries: 2, .. }]
    ));
    let (stale_probe, _) = e.query("alpha", &QueryOptions::default()).unwrap();
    assert_eq!(stale_probe.len(), 1);
    gate.store(true, Ordering::Relaxed);

    let events = lifecycle_until(&rx, label, |ev| {
        matches!(ev, EngineEvent::VolumeReady { .. })
    });
    match events.as_slice() {
        [
            EngineEvent::RescanStarted { .. },
            // Root + beta.txt + gamma.txt.
            EngineEvent::VolumeReady { entries: 3, .. },
        ] => {}
        other => panic!("exact order must be RescanStarted then Ready(3): {other:?}"),
    }
    assert_eq!(
        e.metrics().counters.journal_rescans.load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        store.removed.load(Ordering::Relaxed),
        1,
        "the dead journal's snapshot must be dropped"
    );
    assert_eq!(phase_of(&e, label), VolumeState::Ready);
    // Structural replacement: the rebuilt index inherits prev+1, open
    // handles answer Stale.
    let stats = e.index_stats();
    let s = stats.iter().find(|s| s.volume == label).unwrap();
    assert_eq!(s.structural_generation, 1);
    assert!(matches!(
        stale_probe.page(0, 10),
        Err(super::EngineError::Stale)
    ));

    e.shutdown();
    // Stop-save persists the second incarnation's checkpoint: the restore
    // repositioned the cursor to the snapshot's next_usn (400) under
    // journal id 8.
    assert_eq!(store.saved.lock().as_slice(), &[(8, 400)]);
}

/// Snapshot save failure (flush and stop-save) → counted + the engine
/// keeps serving; nothing panics, nothing goes Failed.
#[test]
fn save_failure_is_counted_and_engine_survives() {
    let label = "FMFWK3:";
    let (_dir, e) = test_engine();
    let rx = sink_channel(&e);
    let store = FakeStore::new(
        vec![LoadScript::Found(
            Box::new(vol(label, &["alpha.txt"])),
            7,
            50,
        )],
        true,
    );
    let journal = FakeJournal::new(
        vec![Incarnation {
            journal_id: 7,
            next_usn: 100,
            view: Some(JournalView {
                journal_id: 7,
                first_usn: 10,
            }),
            reads: vec![],
        }],
        true,
        Arc::new(AtomicU64::new(0)),
    );
    e.spawn_worker_with_seams(label, store.clone(), journal);
    lifecycle_until(&rx, label, |ev| {
        matches!(ev, EngineEvent::VolumeReady { .. })
    });

    // Dirty (never saved) + Ready + checkpoint present → flush tries and
    // fails; the failure is counted and excluded from the saved total.
    assert_eq!(e.flush(), 0);
    assert_eq!(
        e.metrics()
            .counters
            .snapshot_save_failures
            .load(Ordering::Relaxed),
        1
    );
    // The engine continues: still Ready, still answering queries.
    assert_eq!(phase_of(&e, label), VolumeState::Ready);
    let (r, _) = e.query("alpha", &QueryOptions::default()).unwrap();
    assert_eq!(r.len(), 1);

    // The stop-save on shutdown fails too — counted, and shutdown still
    // completes (the worker exits cleanly).
    e.shutdown();
    assert_eq!(
        e.metrics()
            .counters
            .snapshot_save_failures
            .load(Ordering::Relaxed),
        2
    );
    assert_eq!(store.save_attempts.load(Ordering::Relaxed), 2);
    assert!(store.saved.lock().is_empty());
}

/// A storm of failed size/mtime lookups (files vanishing faster than we
/// can stat them) → every failure counted, every batch still applied, and
/// the entries land with zeroed stats instead of being dropped.
#[test]
fn stat_fetch_failure_storm_counts_and_batches_still_apply() {
    let label = "FMFWK4:";
    let (_dir, e) = test_engine();
    let rx = sink_channel(&e);
    let store = FakeStore::new(
        vec![LoadScript::Found(
            Box::new(vol(label, &["note.txt"])),
            7,
            50,
        )],
        false,
    );
    let storm = vec![
        usn_create(200, "new0.txt"),
        usn_create(201, "new1.txt"),
        usn_create(202, "new2.txt"),
    ];
    let late = vec![usn_create(210, "late.txt")];
    let journal = FakeJournal::new(
        vec![Incarnation {
            journal_id: 7,
            next_usn: 100,
            view: Some(JournalView {
                journal_id: 7,
                first_usn: 10,
            }),
            reads: vec![FakeRead::Batch(storm), FakeRead::Batch(late)],
        }],
        false, // every stat lookup fails
        Arc::new(AtomicU64::new(0)),
    );
    e.spawn_worker_with_seams(label, store.clone(), journal);
    lifecycle_until(&rx, label, |ev| {
        matches!(ev, EngineEvent::VolumeReady { .. })
    });

    // 3 failures from the storm batch + 1 from the batch after it — the
    // worker kept applying.
    wait_counter(
        &e.metrics().counters.stat_fetch_failures,
        4,
        "stat_fetch_failures",
    );
    // The change still reached the UI side (debounced IndexChanged).
    lifecycle_until(&rx, label, |ev| {
        matches!(ev, EngineEvent::IndexChanged { .. })
    });
    // Both batches land in the index: root + note.txt restored, then 4
    // created. (Polled: the counter is bumped a hair before the scanned
    // figure is published.)
    let deadline = Instant::now() + WAIT;
    let scanned = loop {
        let scanned = e
            .status()
            .iter()
            .find(|(v, _, _)| v == label)
            .map(|(_, _, n)| *n)
            .unwrap();
        if scanned == 6 || Instant::now() >= deadline {
            break scanned;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(scanned, 6);
    let (r, _) = e.query("late", &QueryOptions::default()).unwrap();
    let rows = r.page(0, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].size, rows[0].mtime),
        (0, 0),
        "failed stat + no prior entry → zeroed stats, not a dropped entry"
    );
    assert_eq!(phase_of(&e, label), VolumeState::Ready);
    e.shutdown();
}
