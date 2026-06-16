//! Elevated end-to-end: a real fmf-service child process over a real C:
//! scan — durability (periodic flush survives a kill) and snapshot restore.
//! Gated like every real-volume test: `#[ignore]` + `FMF_ADMIN_TESTS=1`
//! (`just test-admin`, elevated).

use std::io::Write as _;
use std::time::{Duration, Instant};

use fmf_core::index::testutil::TestDir;
use fmf_proto::frame::{FLAG_EVENT, FrameHeader, read_frame, write_frame};
use fmf_proto::messages::{self, opcode};
use fmf_proto::{PROTOCOL_VERSION, codes};
use fmf_service::pipe::PipeStream;

fn admin_gate() -> bool {
    if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
        eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
        return false;
    }
    true
}

struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_service(data_dir: &std::path::Path, pipe_name: &str) -> Child {
    Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_fmf-service"))
            .args([
                "run",
                "--pipe-name",
                pipe_name,
                "--data-dir",
                &data_dir.to_string_lossy(),
            ])
            .spawn()
            .expect("spawn fmf-service"),
    )
}

fn connect_with_retry(pipe_name: &str, deadline: Duration) -> PipeStream {
    let begin = Instant::now();
    loop {
        if let Ok(s) = PipeStream::connect(pipe_name) {
            return s;
        }
        assert!(begin.elapsed() < deadline, "pipe never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn request(s: &mut PipeStream, id: u32, op: u16, payload: &[u8]) -> (FrameHeader, Vec<u8>) {
    write_frame(
        s,
        FrameHeader {
            len: 0,
            opcode: op,
            flags: 0,
            request_id: id,
            status: 0,
        },
        payload,
    )
    .expect("write");
    loop {
        let (h, p) = read_frame(s).expect("read");
        if h.flags & FLAG_EVENT != 0 {
            continue;
        }
        assert_eq!(h.request_id, id);
        return (h, p);
    }
}

fn hello(s: &mut PipeStream, id: u32) {
    let (h, _) = request(
        s,
        id,
        opcode::HELLO,
        &messages::HelloReq {
            protocol_version: PROTOCOL_VERSION,
        }
        .encode(),
    );
    assert_eq!(h.status, codes::OK);
}

/// Polls `IndexStatus` until C: is Ready; returns the entry count.
fn wait_ready(s: &mut PipeStream, next_id: &mut u32, deadline: Duration) -> u64 {
    let begin = Instant::now();
    loop {
        *next_id += 1;
        let (h, p) = request(s, *next_id, opcode::INDEX_STATUS, &[]);
        assert_eq!(h.status, codes::OK);
        let status: Vec<messages::VolumeStatusWire> =
            messages::decode_json("IndexStatus", &p).expect("decode IndexStatus");
        if let Some(v) = status.iter().find(|v| v.volume == "C:") {
            assert_ne!(v.state, 3, "C: indexing failed");
            if v.state == 1 {
                return v.entries;
            }
        }
        assert!(
            begin.elapsed() < deadline,
            "C: not Ready within {deadline:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "requires elevation + a real C: scan; gated by FMF_ADMIN_TESTS=1 (just test-admin)"]
fn service_e2e_flush_survives_kill_and_restores() {
    if !admin_gate() {
        return;
    }
    // Fresh per-run data dir (TestDir) → guaranteed full-scan cold start.
    let data_dir = TestDir::new();
    // Short flush interval: durability must not depend on a graceful stop.
    let mut f = std::fs::File::create(data_dir.join("service.json")).unwrap();
    f.write_all(br#"{ "volumes": ["C:"], "flush_interval_secs": 10 }"#)
        .unwrap();
    drop(f);
    let pipe_name = format!(r"\\.\pipe\fmf-svc-e2e-{}", std::process::id());

    // 1. Cold start: full scan → Ready → queries answer.
    let child = spawn_service(data_dir.path(), &pipe_name);
    let mut s = connect_with_retry(&pipe_name, Duration::from_secs(30));
    let mut id = 0u32;
    hello(&mut s, 0);
    let entries = wait_ready(&mut s, &mut id, Duration::from_mins(10));
    assert!(entries > 10_000, "suspiciously small C: index: {entries}");

    id += 1;
    let (h, p) = request(
        &mut s,
        id,
        opcode::QUERY,
        &messages::encode_query_req(messages::FmfQueryOptions::default(), "windows"),
    );
    assert_eq!(h.status, codes::OK);
    let (head, _) = messages::QueryRespHead::decode(&p).unwrap();
    assert!(head.count > 0, "'windows' must match something on C:");

    // 2. Wait out a periodic flush, then kill hard (no graceful stop).
    let snapshot = data_dir.join("index").join("c.fmfidx");
    let begin = Instant::now();
    while !snapshot.exists() {
        assert!(
            begin.elapsed() < Duration::from_mins(1),
            "periodic flush never wrote {}",
            snapshot.display()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    drop(child); // kill -9 equivalent — durability is the periodic flush
    drop(s);

    // 3. Restart: must come up from the snapshot (fast Ready), not a rescan.
    let _child2 = spawn_service(data_dir.path(), &pipe_name);
    let mut s2 = connect_with_retry(&pipe_name, Duration::from_secs(30));
    let mut id2 = 0u32;
    hello(&mut s2, 0);
    let restore_begin = Instant::now();
    let restored = wait_ready(&mut s2, &mut id2, Duration::from_mins(1));
    let ready_in = restore_begin.elapsed();
    assert!(restored > 10_000);
    // The M2 gate is restore→ready ≤2s engine-side; over a child process +
    // USN replay we allow process startup slack but a full rescan (~2s scan
    // + deferred + journal replay on a warm machine usually lands well past
    // this) must not be the path.
    assert!(
        ready_in < Duration::from_secs(10),
        "restore took {ready_in:?} — did it full-rescan instead of restoring?"
    );

    // 4. Change-to-event latency (M2: USN→UI ≤1s; the engine-side debounce
    //    is 200ms and the pipe push is the only extra hop). Subscribe, touch
    //    a file on C:, expect IndexChanged within the budget — while the
    //    short flush interval keeps periodic flushes firing around it.
    id2 += 1;
    let (h, _) = request(&mut s2, id2, opcode::SUBSCRIBE, &[]);
    assert_eq!(h.status, codes::OK);
    let tickle = std::env::temp_dir().join(format!("fmf-usn-latency-{}.tmp", std::process::id()));
    let touched = Instant::now();
    std::fs::write(&tickle, b"tick").unwrap();
    let deadline = Duration::from_secs(5); // budget 1s; CI slack on top
    let mut latency = None;
    while touched.elapsed() < deadline {
        let (eh, body) = read_frame(&mut s2).expect("event stream");
        if eh.flags & FLAG_EVENT != 0 {
            let ev = messages::decode_event(&body).unwrap();
            if ev.kind == 3 {
                latency = Some(touched.elapsed());
                break;
            }
        }
    }
    let _ = std::fs::remove_file(&tickle);
    let latency = latency.expect("IndexChanged never arrived");
    assert!(
        latency < Duration::from_secs(1),
        "USN→event took {latency:?} (budget 1s)"
    );
    eprintln!(
        "M2 gate record: restore→ready {ready_in:?} (incl. process spawn), USN→event {latency:?}"
    );
    // data_dir (TestDir) drops last — after _child2 is killed on drop.
}
