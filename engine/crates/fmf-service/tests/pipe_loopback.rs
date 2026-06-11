//! Unelevated loopback tests: a real named pipe (unique name per test), the
//! real server, an injected Ready volume — no real volume, no admin. The
//! byte-level expectations mirror docs/ARCHITECTURE.md「Pipe プロトコル」;
//! the C# client test suite pins the same golden frames.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fmf_core::engine::{Engine, EngineConfig};
use fmf_core::index::testutil::TestDir;
use fmf_core::index::{RawEntry, VolumeIndexBuilder};
use fmf_proto::frame::{FLAG_EVENT, FLAG_RESPONSE, FrameHeader, read_frame, write_frame};
use fmf_proto::messages::{self, opcode};
use fmf_proto::{PROTOCOL_VERSION, codes};
use fmf_service::pipe::PipeStream;
use fmf_service::server::{Server, ServerOptions};

fn unique_name(tag: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        r"\\.\pipe\fmf-test-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn test_engine() -> (TestDir, Arc<Engine>) {
    let dir = TestDir::new();
    let e = Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("engine");
    let mut b = VolumeIndexBuilder::new("C:", 5);
    let alpha: Vec<u16> = "alpha.txt".encode_utf16().collect();
    b.push(RawEntry {
        record: 100,
        parent_record: 5,
        frn: (1 << 48) | 100,
        name_utf16: &alpha,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 1234,
        mtime: 777,
    });
    let beta: Vec<u16> = "beta.log".encode_utf16().collect();
    b.push(RawEntry {
        record: 101,
        parent_record: 5,
        frn: (1 << 48) | 101,
        name_utf16: &beta,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: 99,
        mtime: 888,
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
        // The instance may not exist yet right after Server::start — retry
        // briefly (the accept loop creates it asynchronously).
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
        let resp = messages::HelloResp::decode(&p).unwrap();
        assert_eq!(resp.protocol_version, PROTOCOL_VERSION);
        assert_eq!(resp.server_pid, std::process::id());
        c
    }

    /// Sends one request and waits for its response, buffering any event
    /// pushes that arrive in between.
    fn request(&mut self, op: u16, payload: &[u8]) -> (FrameHeader, Vec<u8>) {
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
        let (head, trace) = messages::QueryRespHead::decode(&p).unwrap();
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
    assert_eq!(row.frn, (1 << 48) | 100);
    assert_eq!(row.size, 1234);
    assert_eq!(row.mtime, 777);
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
    let hx = start("oversize", false);
    let mut c = Client::hello(&hx.pipe_name);
    // Hand-built header announcing a payload over the cap (write_frame
    // refuses to build this, so write raw bytes).
    let mut raw = [0u8; 16];
    raw[0..4].copy_from_slice(&(fmf_proto::frame::MAX_PAYLOAD_LEN + 1).to_le_bytes());
    raw[4..6].copy_from_slice(&opcode::QUERY.to_le_bytes());
    use std::io::Write;
    c.stream.write_all(&raw).unwrap();
    assert!(read_frame(&mut c.stream).is_err(), "connection must die");

    // The fact is on the counters (黙らない): visible over a fresh connection.
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
fn subscribe_receives_engine_events() {
    let hx = start("events", false);
    let mut c = Client::hello(&hx.pipe_name);
    let (h, _) = c.request(opcode::SUBSCRIBE, &[]);
    assert_eq!(h.status, codes::OK);

    // An invalid volume label fails fast in the volume thread → a
    // VolumeFailed (kind 5) push, no elevation needed.
    hx.engine.index_start(&["?:".to_string()]);
    let (eh, body) = c.next_event();
    assert_eq!(eh.flags & FLAG_EVENT, FLAG_EVENT);
    assert_eq!(eh.request_id, 0);
    let ev = messages::decode_event(&body).unwrap();
    assert_eq!(ev.kind, 5, "VolumeFailed");
    assert_eq!(ev.volume_str(), "?:");
    assert_eq!(eh.opcode as u32, ev.kind, "opcode mirrors the event kind");

    // Unsubscribe is idempotent bookkeeping — the connection still serves.
    let (h, _) = c.request(opcode::UNSUBSCRIBE, &[]);
    assert_eq!(h.status, codes::OK);
    let (status, _) = c.query("alpha");
    assert_eq!(status, codes::OK);
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
fn flush_opcode_is_reserved() {
    let hx = start("flushres", false);
    let mut c = Client::hello(&hx.pipe_name);
    let (h, detail) = c.request(opcode::FLUSH_RESERVED, &[]);
    assert_eq!(h.status, codes::INVALID_ARG);
    assert!(String::from_utf8_lossy(&detail).contains("reserved"));
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
    // 遅延予算 (ARCHITECTURE.md): ResultPage 64行 p99 ≤5ms。ループバックの
    // RTT は通常 ~0.1-0.3ms — 5ms はサーマルドリフト下でも余裕の絶対線で、
    // ここで破れたら設計の問題(直列化・コピー過多)を疑う。
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
