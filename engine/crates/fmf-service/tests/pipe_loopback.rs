//! Unelevated loopback tests: a real named pipe (unique name per test), the
//! real server, an injected Ready volume — no real volume, no admin. The
//! byte-level expectations are the server-side half of the contract that the
//! C# client suite pins from the other side, against the same golden frames.
//!
//! These run unconditionally under `just test`, which is the point: the wire
//! is the part most likely to drift and the part least in need of elevation.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fmf_core::engine::{Engine, EngineConfig, EngineEvent};
use fmf_core::index::testutil::TestDir;
use fmf_core::index::{Frn, RawEntry, VolumeIndexBuilder};
use fmf_proto::frame::{FLAG_EVENT, FLAG_RESPONSE, FrameHeader, read_frame, write_frame};
use fmf_proto::messages::{self, opcode};
use fmf_proto::{PROTOCOL_VERSION, codes};
use fmf_service::pipe::PipeStream;
use fmf_service::server::{REQUEST_QUEUE_CAP, Server, ServerOptions};

fn unique_name(tag: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        r"\\.\pipe\fmf-test-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

// NTFS FRN layout: sequence number in the high 16 bits, record number low.
const fn frn(seq: u64, record: u64) -> u64 {
    (seq << 48) | record
}

// Real, second-aligned FILETIMEs that round-trip through the u32-seconds
// mtime column (ADR-0031); pre-1970 small ints collapse to the 0 sentinel.
const MT_ALPHA: i64 = 132_854_688_000_000_000; // ≈ 2022-01-01
const MT_BETA: i64 = 133_170_048_000_000_000; // ≈ 2023-01-01

fn test_engine() -> (TestDir, Arc<Engine>) {
    let dir = TestDir::new();
    let e = Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("engine");
    let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
    let alpha: Vec<u16> = "alpha.txt".encode_utf16().collect();
    b.push(RawEntry {
        parent_frn: Frn(5),
        frn: Frn(frn(1, 100)),
        name_utf16: &alpha,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 1234,
        mtime: MT_ALPHA,
    });
    let beta: Vec<u16> = "beta.log".encode_utf16().collect();
    b.push(RawEntry {
        parent_frn: Frn(5),
        frn: Frn(frn(1, 101)),
        name_utf16: &beta,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 99,
        mtime: MT_BETA,
    });
    e.insert_ready_volume("C:", b.finish());
    (dir, e)
}

struct Harness {
    engine: Arc<Engine>,
    server: Arc<Server>,
    pipe_name: String,
    /// Declared last: the index dir must drop after the engine and server.
    _dir: TestDir,
}

fn start(tag: &str, debug_faults: bool) -> Harness {
    let (dir, engine) = test_engine();
    let pipe_name = unique_name(tag);
    let server = Server::start(
        engine.clone(),
        ServerOptions {
            pipe_name: pipe_name.clone(),
            debug_faults,
            authorized_sids: Vec::new(),
            data_root: None,
            client_verifier: fmf_service::security::verify_client,
        },
    )
    .expect("server start");
    Harness {
        engine,
        server,
        pipe_name,
        _dir: dir,
    }
}

#[test]
fn server_start_rejects_invalid_pipe_security_synchronously() {
    let (_dir, engine) = test_engine();
    let result = Server::start(
        engine.clone(),
        ServerOptions {
            pipe_name: unique_name("bad-sddl"),
            debug_faults: false,
            authorized_sids: vec!["not-a-windows-sid".to_string()],
            data_root: None,
            client_verifier: fmf_service::security::verify_client,
        },
    );
    let Err(error) = result else {
        panic!("invalid pipe SDDL must fail Server::start");
    };
    assert!(error.raw_os_error().is_some());
    engine.set_event_sink(None);
}

#[test]
fn server_start_rejects_a_squatted_pipe_name_synchronously() {
    let first = start("squatted", false);
    let (_dir, engine) = test_engine();
    let result = Server::start(
        engine.clone(),
        ServerOptions {
            pipe_name: first.pipe_name.clone(),
            debug_faults: false,
            authorized_sids: Vec::new(),
            data_root: None,
            client_verifier: fmf_service::security::verify_client,
        },
    );
    let Err(error) = result else {
        panic!("FILE_FLAG_FIRST_PIPE_INSTANCE collision must fail Server::start");
    };
    assert_eq!(error.raw_os_error(), Some(5));
    engine.set_event_sink(None);
}

#[test]
fn server_stop_disconnects_live_clients() {
    let h = start("stop-live", false);
    let mut client = Client::hello(&h.pipe_name);

    h.server.stop();

    let started = std::time::Instant::now();
    read_frame(&mut client.stream).expect_err("server stop must break the client pipe");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "disconnect should be immediate"
    );
}

#[test]
fn server_stop_releases_a_reader_backpressured_by_slow_workers() {
    let (_dir, engine) = test_engine();
    let pipe_name = unique_name("stop-backpressure");
    let server = Server::start(
        engine.clone(),
        ServerOptions {
            pipe_name: pipe_name.clone(),
            debug_faults: true,
            authorized_sids: Vec::new(),
            data_root: None,
            client_verifier: fmf_service::security::verify_client,
        },
    )
    .expect("server start");
    let mut client = Client::hello(&pipe_name);
    let (_, Some((result_id, _))) = client.query("!!lag") else {
        panic!("lag query failed");
    };

    // Both workers enter the 250 ms debug delay, the next REQUEST_QUEUE_CAP
    // frames fill the bounded queue, and the final frame leaves the reader
    // waiting in the cancellation-aware send.
    for request_id in 0..(REQUEST_QUEUE_CAP + 3) {
        write_frame(
            &mut client.stream,
            FrameHeader {
                len: 0,
                opcode: opcode::RESULT_PAGE,
                flags: 0,
                request_id: 10_000 + request_id as u32,
                status: 0,
            },
            &messages::ResultPageReq {
                result_id,
                offset: 0,
                count: 1,
            }
            .encode(),
        )
        .expect("pipeline slow request");
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    let started = std::time::Instant::now();
    server.stop();
    server.join();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "disconnect must cancel a reader waiting on the full request queue"
    );
    engine.set_event_sink(None);
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.stop();
        self.engine.set_event_sink(None);
    }
}

struct Client {
    stream: PipeStream,
    next_id: u32,
    events: VecDeque<(FrameHeader, Vec<u8>)>,
}

impl Client {
    fn connect(pipe_name: &str) -> Self {
        // Server::start now returns only after the first instance is listening;
        // keep a short retry for scheduler noise between test processes.
        for _ in 0..100 {
            if let Ok(stream) = PipeStream::connect(pipe_name) {
                return Self {
                    stream,
                    next_id: 1,
                    events: VecDeque::new(),
                };
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("pipe {pipe_name} never came up");
    }

    fn hello(pipe_name: &str) -> Self {
        let mut c = Self::connect(pipe_name);
        let (h, p) = c.request(
            opcode::HELLO,
            &messages::HelloReq {
                protocol_version: PROTOCOL_VERSION,
            }
            .encode(),
        );
        assert_eq!(h.status, codes::OK);
        let resp = messages::HelloResp::decode(&p).expect("decode HelloResp");
        assert_eq!(resp.protocol_version, PROTOCOL_VERSION);
        assert_eq!(resp.server_pid, std::process::id());
        c
    }

    /// Writes one request frame and returns its `request_id` *without*
    /// waiting for the response. Needed wherever the test has to keep several
    /// requests in flight (pipelining, worker parking); the reply is picked up
    /// later with [`Client::await_response`].
    fn send(&mut self, op: u16, payload: &[u8]) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(
            &mut self.stream,
            FrameHeader {
                len: 0,
                opcode: op,
                flags: 0,
                request_id: id,
                status: 0,
            },
            payload,
        )
        .expect("write request");
        id
    }

    /// Writes the one-way `QueryCancel` control frame (opcode 13, ADR-0044):
    /// zero payload, no response, and `request_id` naming the query request to
    /// abort — so, unlike [`Client::send`], it does not consume a fresh id.
    fn send_query_cancel(&mut self, request_id: u32) {
        write_frame(
            &mut self.stream,
            FrameHeader {
                len: 0,
                opcode: opcode::QUERY_CANCEL,
                flags: 0,
                request_id,
                status: 0,
            },
            &[],
        )
        .expect("write QueryCancel");
    }

    /// Reads until the response for `request_id` arrives, buffering event
    /// pushes and discarding the pipelined replies of other requests (the two
    /// workers answer out of order by design).
    fn await_response(&mut self, request_id: u32) -> (FrameHeader, Vec<u8>) {
        loop {
            let (h, p) = read_frame(&mut self.stream).expect("read response");
            if h.flags & FLAG_EVENT != 0 {
                self.events.push_back((h, p));
                continue;
            }
            assert_eq!(h.flags & FLAG_RESPONSE, FLAG_RESPONSE);
            if h.request_id == request_id {
                return (h, p);
            }
        }
    }

    /// Sends one request and waits for its response, buffering any event
    /// pushes that arrive in between. Strict: nothing else may be in flight,
    /// so any other response frame is a correlation bug.
    fn request(&mut self, op: u16, payload: &[u8]) -> (FrameHeader, Vec<u8>) {
        let id = self.send(op, payload);
        loop {
            let (h, p) = read_frame(&mut self.stream).expect("read response");
            if h.flags & FLAG_EVENT != 0 {
                self.events.push_back((h, p));
                continue;
            }
            assert_eq!(h.flags & FLAG_RESPONSE, FLAG_RESPONSE);
            assert_eq!(h.request_id, id, "responses correlate by request_id");
            assert_eq!(h.opcode, op);
            return (h, p);
        }
    }

    /// Next event push (buffered or read fresh — blocking; the test runner's
    /// timeout is the watchdog).
    fn next_event(&mut self) -> (FrameHeader, Vec<u8>) {
        if let Some(ev) = self.events.pop_front() {
            return ev;
        }
        let (h, p) = read_frame(&mut self.stream).expect("read event");
        assert_ne!(
            h.flags & FLAG_EVENT,
            0,
            "unexpected non-event frame while waiting for an event"
        );
        (h, p)
    }

    fn query(&mut self, text: &str) -> (i32, Option<(u64, u64)>) {
        let (h, p) = self.request(
            opcode::QUERY,
            &messages::encode_query_req(messages::FmfQueryOptions::default(), text),
        );
        if h.status != codes::OK {
            return (h.status, None);
        }
        let (head, trace) = messages::QueryRespHead::decode(&p).expect("decode QueryRespHead");
        assert!(!trace.is_empty(), "QueryTrace JSON rides along");
        (h.status, Some((head.result_id, head.count)))
    }

    fn page(&mut self, result_id: u64, offset: u64, count: u32) -> (i32, Vec<u8>) {
        let (h, p) = self.request(
            opcode::RESULT_PAGE,
            &messages::ResultPageReq {
                result_id,
                offset,
                count,
            }
            .encode(),
        );
        (h.status, p)
    }
}

#[test]
fn hello_query_page_free_roundtrip() {
    let hx = start("roundtrip", false);
    let mut c = Client::hello(&hx.pipe_name);

    let (status, Some((rid, count))) = c.query("alpha") else {
        panic!("query failed");
    };
    assert_eq!(status, codes::OK);
    assert_eq!(count, 1);

    let (status, body) = c.page(rid, 0, 16);
    assert_eq!(status, codes::OK);
    let page = messages::decode_page(&body).unwrap();
    assert_eq!(page.rows.len(), 1);
    let row = page.rows[0];
    assert_eq!(row.entry_ref >> 32, 0, "volume ordinal in the high half");
    assert_eq!(row.frn, frn(1, 100));
    assert_eq!(row.size, 1234);
    assert_eq!(row.mtime, MT_ALPHA);
    let name = &page.blob[row.name_off as usize..row.name_off as usize + row.name_len as usize];
    assert_eq!(name, b"alpha.txt");
    let parent = &page.blob
        [row.parent_path_off as usize..row.parent_path_off as usize + row.parent_path_len as usize];
    assert_eq!(parent, b"C:\\");

    // Out-of-range pages are empty, not errors (FFI parity).
    let (status, body) = c.page(rid, 999, 16);
    assert_eq!(status, codes::OK);
    assert_eq!(messages::decode_page(&body).unwrap().rows.len(), 0);

    // Free → the id is gone; further pages answer the evicted-or-unknown STALE.
    let (h, _) = c.request(opcode::RESULT_FREE, &messages::encode_result_free(rid));
    assert_eq!(h.status, codes::OK);
    let (status, detail) = c.page(rid, 0, 1);
    assert_eq!(status, codes::STALE);
    assert!(String::from_utf8_lossy(&detail).contains("evicted or unknown"));
}

#[test]
fn hello_version_mismatch_is_refused() {
    let hx = start("vermismatch", false);
    let mut c = Client::connect(&hx.pipe_name);
    let (h, detail) = c.request(
        opcode::HELLO,
        &messages::HelloReq {
            protocol_version: 99,
        }
        .encode(),
    );
    assert_eq!(h.status, codes::INVALID_ARG);
    assert!(String::from_utf8_lossy(&detail).contains("mismatch"));
}

#[test]
fn request_before_hello_drops_the_connection() {
    let hx = start("nohello", false);
    let mut c = Client::connect(&hx.pipe_name);
    write_frame(
        &mut c.stream,
        FrameHeader {
            len: 0,
            opcode: opcode::QUERY,
            flags: 0,
            request_id: 1,
            status: 0,
        },
        &messages::encode_query_req(messages::FmfQueryOptions::default(), "x"),
    )
    .unwrap();
    assert!(
        read_frame(&mut c.stream).is_err(),
        "server must disconnect instead of serving an un-greeted client"
    );
}

#[test]
fn oversized_frame_disconnects_and_counts() {
    use std::io::Write;

    let hx = start("oversize", false);
    let mut c = Client::hello(&hx.pipe_name);
    // Hand-built header announcing a payload over the cap (write_frame
    // refuses to build this, so write raw bytes).
    let mut raw = [0u8; 16];
    raw[0..4].copy_from_slice(&(fmf_proto::frame::MAX_PAYLOAD_LEN + 1).to_le_bytes());
    raw[4..6].copy_from_slice(&opcode::QUERY.to_le_bytes());
    c.stream.write_all(&raw).unwrap();
    assert!(read_frame(&mut c.stream).is_err(), "connection must die");

    // The fact is on the counters (don't go silent): visible over a fresh connection.
    let mut c2 = Client::hello(&hx.pipe_name);
    let (h, body) = c2.request(opcode::STATS, &[]);
    assert_eq!(h.status, codes::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["counters"]["pipe_malformed_frames"].as_u64().unwrap() >= 1,
        "malformed frame must be counted: {json}"
    );
}

#[test]
fn operation_specific_caps_disconnect_before_reading_the_announced_body() {
    use std::io::Write;

    let harness = start("operation-caps", false);
    for (opcode, announced_len) in [
        (opcode::STATS, 1),
        // QueryCancel is zero-payload by contract (ADR-0044); a body announced
        // on it is as malformed as one on Stats.
        (opcode::QUERY_CANCEL, 1),
        (
            opcode::QUERY,
            messages::FmfQueryOptions::LEN as u32 + fmf_proto::limits::MAX_QUERY_BYTES + 1,
        ),
        (u16::MAX, 1),
    ] {
        let mut client = Client::hello(&harness.pipe_name);
        let header = FrameHeader {
            len: announced_len,
            opcode,
            flags: 0,
            request_id: 99,
            status: 0,
        };
        client.stream.write_all(&header.to_bytes()).unwrap();
        assert!(
            read_frame(&mut client.stream).is_err(),
            "opcode {opcode} must be rejected from its header without waiting for a body"
        );
    }
}

#[test]
fn subscribe_receives_engine_events() {
    let hx = start("events", false);
    let mut c = Client::hello(&hx.pipe_name);
    let (h, _) = c.request(opcode::SUBSCRIBE, &[]);
    assert_eq!(h.status, codes::OK);

    // Drive the real event fan-out deterministically without abusing
    // IndexStart: invalid volume selections are synchronous INVALID_ARG and
    // intentionally create no worker or VolumeFailed event.
    hx.engine.emit_test_event(EngineEvent::VolumeFailed {
        volume: "C:".to_string(),
        message: "scripted".to_string(),
    });
    let (eh, body) = c.next_event();
    assert_eq!(eh.flags & FLAG_EVENT, FLAG_EVENT);
    assert_eq!(eh.request_id, 0);
    let ev = messages::decode_event(&body).unwrap();
    assert_eq!(ev.kind, 5, "VolumeFailed");
    assert_eq!(ev.volume_str(), "C:");
    assert_eq!(
        u32::from(eh.opcode),
        ev.kind,
        "opcode mirrors the event kind"
    );

    // Unsubscribe is idempotent bookkeeping — the connection still serves.
    let (h, _) = c.request(opcode::UNSUBSCRIBE, &[]);
    assert_eq!(h.status, codes::OK);
    let (status, _) = c.query("alpha");
    assert_eq!(status, codes::OK);
}

#[test]
fn index_start_rejects_duplicates_and_unavailable_volumes_atomically() {
    let hx = start("index-start-validation", false);
    let mut c = Client::hello(&hx.pipe_name);
    let before: Vec<_> = hx
        .engine
        .status()
        .into_iter()
        .map(|(label, _, _)| label)
        .collect();
    let available = Engine::list_ntfs_volumes();

    if let Some(label) = available.first() {
        let payload = messages::encode_json(
            "IndexStart",
            &messages::IndexStartReq {
                volumes: vec![label.clone(), label.to_ascii_lowercase()],
            },
        )
        .unwrap();
        let (header, _) = c.request(opcode::INDEX_START, &payload);
        assert_eq!(header.status, codes::INVALID_ARG);
        assert_eq!(
            hx.engine
                .status()
                .into_iter()
                .map(|(label, _, _)| label)
                .collect::<Vec<_>>(),
            before,
            "duplicate rejection must happen before any slot or worker is created"
        );
    }

    let unavailable = (b'A'..=b'Z')
        .map(|letter| format!("{}:", char::from(letter)))
        .find(|label| !available.contains(label))
        .expect("a test host cannot have all 26 drive letters as fixed NTFS volumes");
    let mut volumes = available.first().cloned().into_iter().collect::<Vec<_>>();
    volumes.push(unavailable);
    let payload =
        messages::encode_json("IndexStart", &messages::IndexStartReq { volumes }).unwrap();
    let (header, _) = c.request(opcode::INDEX_START, &payload);
    assert_eq!(header.status, codes::INVALID_ARG);
    assert_eq!(
        hx.engine
            .status()
            .into_iter()
            .map(|(label, _, _)| label)
            .collect::<Vec<_>>(),
        before,
        "one unavailable volume must reject the entire request atomically"
    );
}

#[test]
fn result_handles_evict_least_recently_used() {
    let hx = start("evict", false);
    let mut c = Client::hello(&hx.pipe_name);
    let (_, Some((first, _))) = c.query("alpha") else {
        panic!()
    };
    // Keep `first` warm while 64 more results pour in: the LRU victim must
    // be one of the cold ones, not the on-screen handle.
    for i in 0..64 {
        let (status, _) = c.query(&format!("q{i}"));
        assert_eq!(status, codes::OK);
        if i % 16 == 0 {
            let (status, _) = c.page(first, 0, 1);
            assert_eq!(status, codes::OK, "warm handle must survive eviction");
        }
    }
    let (status, _) = c.page(first, 0, 1);
    assert_eq!(status, codes::OK, "LRU keeps the actively used result");
}

#[test]
fn panic_fault_answers_panic_and_the_connection_survives() {
    let hx = start("panic", true);
    let mut c = Client::hello(&hx.pipe_name);
    let (status, none) = c.query("!!panic");
    assert_eq!(status, codes::PANIC);
    assert!(none.is_none());
    // The firewall caught it — same connection keeps working.
    let (status, _) = c.query("alpha");
    assert_eq!(status, codes::OK);
}

#[test]
fn drop_fault_severs_the_connection() {
    let hx = start("dropfault", true);
    let mut c = Client::hello(&hx.pipe_name);
    write_frame(
        &mut c.stream,
        FrameHeader {
            len: 0,
            opcode: opcode::QUERY,
            flags: 0,
            request_id: 9,
            status: 0,
        },
        &messages::encode_query_req(messages::FmfQueryOptions::default(), "!!drop"),
    )
    .unwrap();
    assert!(read_frame(&mut c.stream).is_err());
}

#[test]
fn page_roundtrip_stays_inside_the_latency_budget() {
    // Latency budget (ADR-0016): ResultPage 64 rows p99 <=5ms. Loopback
    // RTT is normally ~0.1-0.3ms — 5ms is a comfortable absolute line even
    // under thermal drift; breaking it here points to a design problem
    // (serialization, excessive copying).
    let hx = start("latency", false);
    let mut c = Client::hello(&hx.pipe_name);
    let (_, Some((rid, _))) = c.query("alpha") else {
        panic!()
    };
    let mut samples: Vec<std::time::Duration> = (0..200)
        .map(|_| {
            let t = std::time::Instant::now();
            let (status, _) = c.page(rid, 0, 64);
            assert_eq!(status, codes::OK);
            t.elapsed()
        })
        .collect();
    samples.sort();
    let p99 = samples[samples.len() * 99 / 100];
    assert!(
        p99 < std::time::Duration::from_millis(5),
        "ResultPage p99 {p99:?} blew the 5ms budget"
    );
}

#[test]
fn lag_fault_delays_pages_not_queries() {
    let hx = start("lag", true);
    let mut c = Client::hello(&hx.pipe_name);
    let (_, Some((rid, _))) = c.query("!!lag") else {
        panic!()
    };
    let begin = std::time::Instant::now();
    let (status, _) = c.page(rid, 0, 1);
    assert_eq!(status, codes::OK);
    assert!(
        begin.elapsed() >= std::time::Duration::from_millis(240),
        "!!lag pages must stall ~250ms"
    );
}

/// `QueryCancel` (opcode 13, ADR-0044) is the only frame the reader executes
/// itself instead of queueing, so it can overtake a query that is still
/// waiting for a worker. This test creates exactly that situation and pins
/// the effect.
///
/// The observation is a *frame*, not a timeout: a cancelled query is answered
/// with `FMF_E_CANCELLED` + the `query cancelled` detail. Asserting the detail
/// separates the explicit cancel from the latest-query-wins supersede path,
/// which answers the same status with `query superseded`.
///
/// Both workers are parked in the 250 ms `!!lag` page sleep first, so the
/// target query is still queued — not dispatched — when the cancel frame is
/// read. The bounded retry covers only the one thing the test cannot control,
/// the OS scheduling the workers into that sleep; a cancel that does nothing
/// answers `OK` on every attempt and still fails in bounded time.
#[test]
fn query_cancel_aborts_a_query_that_is_still_queued() {
    let hx = start("query-cancel", true);
    let mut c = Client::hello(&hx.pipe_name);
    let (_, Some((lagged, _))) = c.query("!!lag") else {
        panic!("lag query failed");
    };

    for _ in 0..3 {
        // One lagged page per worker (REQUEST_QUEUE_CAP is one slot per
        // worker, so it is also the worker count). Each dequeue costs the
        // worker a 250 ms sleep inside result_page.
        for _ in 0..REQUEST_QUEUE_CAP {
            c.send(
                opcode::RESULT_PAGE,
                &messages::ResultPageReq {
                    result_id: lagged,
                    offset: 0,
                    count: 1,
                }
                .encode(),
            );
        }
        // The workers only have to be *scheduled*; they then hold the sleep
        // for 250 ms, which is the margin the two frames below race against.
        std::thread::sleep(std::time::Duration::from_millis(100));

        let query_id = c.send(
            opcode::QUERY,
            &messages::encode_query_req(messages::FmfQueryOptions::default(), "alpha"),
        );
        c.send_query_cancel(query_id);

        let (header, detail) = c.await_response(query_id);
        if header.status == codes::CANCELLED {
            assert_eq!(
                String::from_utf8_lossy(&detail),
                "query cancelled",
                "explicit QueryCancel, not the latest-query-wins supersede path"
            );
            return;
        }
        assert_eq!(
            header.status,
            codes::OK,
            "a query is either served or cancelled"
        );
    }
    panic!("QueryCancel never aborted a query that was still queued behind parked workers");
}

/// The reader executes `QueryCancel` before the dispatcher's handshake gate
/// can see it, so the gate is duplicated there. An un-greeted client must be
/// disconnected rather than allowed to poke the query registry.
#[test]
fn query_cancel_before_hello_drops_the_connection() {
    let hx = start("cancel-nohello", false);
    let mut c = Client::connect(&hx.pipe_name);
    c.send_query_cancel(1);

    // Follow with a request the server *would* answer if it had not hung up.
    // That turns a missing gate into an immediate failed assertion instead of
    // a blocking read only the harness timeout could end. The write itself may
    // already fail against the severed pipe — that is the passing case too.
    let _ = write_frame(
        &mut c.stream,
        FrameHeader {
            len: 0,
            opcode: opcode::HELLO,
            flags: 0,
            request_id: 2,
            status: 0,
        },
        &messages::HelloReq {
            protocol_version: PROTOCOL_VERSION,
        }
        .encode(),
    );
    if let Ok((header, _)) = read_frame(&mut c.stream) {
        panic!(
            "server must disconnect instead of serving an un-greeted client; \
             got opcode {} status {}",
            header.opcode, header.status
        );
    }
}

/// One-way means one-way: a `QueryCancel` naming a request id that was never
/// issued — or one that already completed — must write nothing back and must
/// not disturb the connection. `request` asserts the next response correlates
/// to *its* id and opcode, so a stray reply to any of the four control frames
/// below fails the test.
#[test]
fn query_cancel_without_a_live_query_is_a_silent_no_op() {
    let hx = start("cancel-unknown", false);
    let mut c = Client::hello(&hx.pipe_name);

    // Never-issued ids: the registry has nothing to cancel.
    c.send_query_cancel(0);
    c.send_query_cancel(u32::MAX);

    let query_id = c.send(
        opcode::QUERY,
        &messages::encode_query_req(messages::FmfQueryOptions::default(), "alpha"),
    );
    let (header, body) = c.await_response(query_id);
    assert_eq!(header.status, codes::OK);
    let (head, _) = messages::QueryRespHead::decode(&body).expect("decode QueryRespHead");
    assert_eq!(head.count, 1, "a stray cancel must not touch other queries");

    // Already finished: the worker's guard removed the lifecycle, so this is
    // the same no-op — and repeating it pins idempotence.
    c.send_query_cancel(query_id);
    c.send_query_cancel(query_id);

    let (status, _) = c.query("beta");
    assert_eq!(status, codes::OK, "the connection keeps serving");
}
