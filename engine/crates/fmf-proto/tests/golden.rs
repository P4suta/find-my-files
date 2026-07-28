//! Golden corpus pin: every representative frame the protocol can carry,
//! captured byte-for-byte in `contract/golden/` (repo root). The C# suite
//! (`GoldenCorpusTests`) pins the very same files — this is the "Rust and C#
//! tests pin the same golden bytes" rule made real (ADR-0018).
//!
//! Re-capture (bless) is an explicit ritual for intentional contract
//! changes only:
//!
//! ```text
//! just contract-bless
//! ```
//!
//! A normal run never writes — it fails on any byte that drifted.

use std::path::PathBuf;

use fmf_proto::frame::{FLAG_EVENT, FLAG_RESPONSE, FrameHeader, HEADER_LEN, decode_header};
use fmf_proto::messages::{
    FmfEvent, FmfQueryOptions, FmfRow, HelloReq, HelloResp, IndexStartReq, QueryRespHead,
    ResultPageReq, ServiceInfoResp, VolumeStatusWire, decode_event, encode_event, encode_json,
    encode_page, encode_query_req, encode_result_free, opcode,
};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden")
}

fn bless_mode() -> bool {
    std::env::var("FMF_BLESS").as_deref() == Ok("1")
}

/// One contiguous frame exactly as it crosses the pipe.
fn frame(opcode: u16, flags: u16, request_id: u32, status: i32, payload: &[u8]) -> Vec<u8> {
    let header = FrameHeader {
        len: payload.len() as u32,
        opcode,
        flags,
        request_id,
        status,
    };
    let mut v = Vec::with_capacity(HEADER_LEN + payload.len());
    v.extend_from_slice(&header.to_bytes());
    v.extend_from_slice(payload);
    v
}

struct Case {
    file: &'static str,
    desc: &'static str,
    bytes: Vec<u8>,
}

/// The corpus, constructed from the current codec. Bless captures these
/// bytes; a pin run requires the files to match them exactly.
fn corpus() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut case = |file: &'static str, desc: &'static str, bytes: Vec<u8>| {
        cases.push(Case { file, desc, bytes });
    };

    // ── Hello (op 1) ────────────────────────────────────────────────────
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "golden manifest descriptor text, never a format string"
    )]
    case(
        "hello_req.bin",
        "op1 request: HelloReq{protocol_version:1}",
        frame(
            opcode::HELLO,
            0,
            1,
            0,
            &HelloReq {
                protocol_version: 1,
            }
            .encode(),
        ),
    );
    case(
        "hello_resp.bin",
        "op1 response: HelloResp{protocol_version:1, abi_version:1, server_pid:0x04030201}",
        frame(
            opcode::HELLO,
            FLAG_RESPONSE,
            1,
            0,
            &HelloResp {
                protocol_version: 1,
                abi_version: 1,
                server_pid: 0x0403_0201,
            }
            .encode(),
        ),
    );

    // ── Subscribe / Unsubscribe (op 2/3, empty payloads) ────────────────
    case(
        "subscribe_req.bin",
        "op2 request: empty payload",
        frame(opcode::SUBSCRIBE, 0, 2, 0, &[]),
    );
    case(
        "subscribe_resp.bin",
        "op2 response: empty payload",
        frame(opcode::SUBSCRIBE, FLAG_RESPONSE, 2, 0, &[]),
    );
    case(
        "unsubscribe_req.bin",
        "op3 request: empty payload",
        frame(opcode::UNSUBSCRIBE, 0, 3, 0, &[]),
    );

    // ── ListVolumes / IndexStart / IndexStatus (JSON, snake_case) ───────
    case(
        "list_volumes_resp.bin",
        "op4 response: JSON [{volume,state,entries}] — two volumes, Ready+Scanning",
        frame(
            opcode::LIST_VOLUMES,
            FLAG_RESPONSE,
            4,
            0,
            &encode_json(
                "ListVolumes",
                &vec![
                    VolumeStatusWire {
                        volume: "C:".into(),
                        state: 1,
                        entries: 1_268_560,
                    },
                    VolumeStatusWire {
                        volume: "D:".into(),
                        state: 0,
                        entries: 0,
                    },
                ],
            )
            .expect("encode golden fixture"),
        ),
    );
    case(
        "index_start_req.bin",
        "op5 request: JSON {volumes:[C:,D:]}",
        frame(
            opcode::INDEX_START,
            0,
            5,
            0,
            &encode_json(
                "IndexStart",
                &IndexStartReq {
                    volumes: vec!["C:".into(), "D:".into()],
                },
            )
            .expect("encode golden fixture"),
        ),
    );
    case(
        "index_status_resp.bin",
        "op6 response: JSON, single volume Rescanning(2)",
        frame(
            opcode::INDEX_STATUS,
            FLAG_RESPONSE,
            6,
            0,
            &encode_json(
                "IndexStatus",
                &vec![VolumeStatusWire {
                    volume: "C:".into(),
                    state: 2,
                    entries: 42,
                }],
            )
            .expect("encode golden fixture"),
        ),
    );

    // ── Query (op 7) ────────────────────────────────────────────────────
    case(
        "query_req_basic.bin",
        "op7 request: default options (Name/Asc/Smart/no-hidden) + ASCII text",
        frame(
            opcode::QUERY,
            0,
            7,
            0,
            &encode_query_req(FmfQueryOptions::default(), "win"),
        ),
    );
    case(
        "query_req_unicode.bin",
        "op7 request: Mtime/Desc/Sensitive/include-hidden/regex(whole+path) + \
         multi-byte UTF-8 text",
        frame(
            opcode::QUERY,
            0,
            8,
            0,
            &encode_query_req(
                FmfQueryOptions {
                    sort: 2,
                    desc: 1,
                    case_mode: 2,
                    include_hidden_system: 1,
                    regex_mode: 3,
                    _reserved: 0,
                    presentation_basis: 0x0102_0304_0506_0708,
                },
                "日本語 ext:txt",
            ),
        ),
    );
    // The trace JSON is opaque to the codec (head + verbatim bytes). The
    // authoritative QueryTrace shape is pinned by query_trace.json
    // (fmf-core's golden_json test), not here.
    case(
        "query_resp.bin",
        "op7 response: QueryRespHead{result_id:1,count:3} + opaque trace JSON",
        frame(
            opcode::QUERY,
            FLAG_RESPONSE,
            7,
            0,
            &QueryRespHead {
                result_id: 1,
                count: 3,
            }
            .encode_with_trace(br#"{"query":"win","unchanged":false}"#),
        ),
    );

    // ── ResultPage (op 8) ───────────────────────────────────────────────
    case(
        "result_page_req.bin",
        "op8 request: {result_id:1, offset:128, count:64}",
        frame(
            opcode::RESULT_PAGE,
            0,
            9,
            0,
            &ResultPageReq {
                result_id: 1,
                offset: 128,
                count: 64,
            }
            .encode(),
        ),
    );
    case(
        "result_page_resp_empty.bin",
        "op8 response: zero rows, empty blob",
        frame(
            opcode::RESULT_PAGE,
            FLAG_RESPONSE,
            9,
            0,
            &encode_page(&[], &[]).expect("empty page encodes"),
        ),
    );
    // Blob layout convention: per row, name bytes then parent bytes,
    // appended in row order with no dedup (what the C# encoder produces;
    // offsets being explicit, any layout is wire-legal — the corpus pins
    // the canonical one). Row 3's name carries a WTF-8 unpaired surrogate
    // (U+D800 = ED A0 80): file names are WTF-8, not UTF-8.
    {
        let mut blob: Vec<u8> = Vec::new();
        let mut rows: Vec<FmfRow> = Vec::new();
        let mut push_row =
            |name: &[u8], parent: &[u8], frn: u64, size: u64, mtime: i64, flags: u32| {
                let name_off = blob.len() as u32;
                blob.extend_from_slice(name);
                let parent_off = blob.len() as u32;
                blob.extend_from_slice(parent);
                rows.push(FmfRow {
                    entry_ref: rows.len() as u64 + 1,
                    frn,
                    size,
                    mtime,
                    name_off,
                    parent_path_off: parent_off,
                    flags,
                    name_len: name.len() as u32,
                    parent_path_len: parent.len() as u32,
                    _reserved: 0,
                });
            };
        push_row(
            b"alpha.txt",
            b"C:\\",
            (1 << 48) | 0x64,
            1234,
            133_500_000_000_000_000,
            0,
        );
        push_row(
            "省察.txt".as_bytes(),
            "C:\\メモ\\".as_bytes(),
            (2 << 48) | 0xC8,
            0x1_0000_0001, // > u32::MAX — pins the u64 size column on the wire
            -5,            // pins i64 signedness
            0,
        );
        let mut surrogate_name = vec![0xED, 0xA0, 0x80]; // unpaired U+D800, WTF-8
        surrogate_name.extend_from_slice(b"tail.dat");
        push_row(&surrogate_name, b"C:\\", (3 << 48) | 0x12C, 0, 0, 1);
        case(
            "result_page_resp_rows.bin",
            "op8 response: 3 rows (ASCII / multi-byte / WTF-8 unpaired surrogate), \
             u64 size overflow, negative mtime, dir flag",
            frame(
                opcode::RESULT_PAGE,
                FLAG_RESPONSE,
                10,
                0,
                &encode_page(&rows, &blob).expect("golden page encodes"),
            ),
        );
    }

    // ── ResultFree (op 9) ───────────────────────────────────────────────
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "golden manifest descriptor text, never a format string"
    )]
    case(
        "result_free_req.bin",
        "op9 request: {result_id:1}",
        frame(opcode::RESULT_FREE, 0, 11, 0, &encode_result_free(1)),
    );

    // ── Event pushes (flags=event, request_id=0, opcode = kind 1..=6) ───
    let events: [(&'static str, &'static str, u32, u64, &str); 6] = [
        (
            "event_progress.bin",
            "kind1 Progress: scanned count",
            1,
            500_000,
            "C:",
        ),
        (
            "event_volume_ready.bin",
            "kind2 VolumeReady: entries",
            2,
            1_268_560,
            "C:",
        ),
        (
            "event_index_changed.bin",
            "kind3 IndexChanged (debounced)",
            3,
            0,
            "C:",
        ),
        (
            "event_rescan_started.bin",
            "kind4 RescanStarted",
            4,
            0,
            "D:",
        ),
        ("event_volume_failed.bin", "kind5 VolumeFailed", 5, 0, "D:"),
        (
            "event_engine_error.bin",
            "kind6 EngineError: entries=severity(2=error)",
            6,
            2,
            "",
        ),
    ];
    for (file, desc, kind, entries, volume) in events {
        case(
            file,
            desc,
            frame(
                kind as u16,
                FLAG_EVENT,
                0,
                0,
                &encode_event(&FmfEvent::new(kind, entries, volume)),
            ),
        );
    }

    // ── Error response (status != 0, UTF-8 detail payload) ──────────────
    case(
        "error_resp.bin",
        "op7 response with status=QUERY_SYNTAX(5): payload is the UTF-8 detail text",
        frame(
            opcode::QUERY,
            FLAG_RESPONSE,
            12,
            fmf_proto::codes::QUERY_SYNTAX,
            "クエリ構文エラー: unbalanced quote at 3".as_bytes(),
        ),
    );

    // ── ServiceInfo (op 12) ─────────────────────────────────────────────
    case(
        "service_info_resp.bin",
        "op12 response: JSON {uptime_ms, connections, version}",
        frame(
            opcode::SERVICE_INFO,
            FLAG_RESPONSE,
            13,
            0,
            &encode_json(
                "ServiceInfo",
                &ServiceInfoResp {
                    uptime_ms: 123_456,
                    connections: 1,
                    version: "0.1.0".into(),
                },
            )
            .expect("encode golden fixture"),
        ),
    );

    cases
}

fn check_file(file: &str, bytes: &[u8]) {
    let path = golden_dir().join(file);
    if bless_mode() {
        std::fs::create_dir_all(golden_dir()).expect("create golden dir");
        std::fs::write(&path, bytes).expect("write golden fixture");
        return;
    }
    let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{file}: cannot read golden file ({e}). Run the bless ritual \
             (FMF_BLESS=1) only for an intentional contract change."
        )
    });
    assert_eq!(
        on_disk, bytes,
        "{file}: golden bytes drifted from the current codec. If the \
         contract change is intentional: update fmf-contract first, then \
         re-capture with `just contract-bless` (FMF_BLESS=1) and regenerate \
         the C# bindings with `just contract-gen` (ADR-0018)."
    );
}

#[test]
fn corpus_pins_the_wire_bytes() {
    for c in corpus() {
        check_file(c.file, &c.bytes);
    }
}

/// The frame corpus must also *decode* back to the values it was built
/// from — encode-only pins would let an asymmetric codec bug through.
#[test]
fn corpus_frames_decode_back() {
    for c in corpus() {
        let header_bytes: [u8; HEADER_LEN] = c.bytes[..HEADER_LEN].try_into().unwrap();
        let header = decode_header(&header_bytes).unwrap();
        let payload = &c.bytes[HEADER_LEN..];
        assert_eq!(header.len as usize, payload.len(), "{}", c.file);

        if header.flags & FLAG_EVENT != 0 {
            let ev = decode_event(payload).unwrap();
            assert_eq!(ev.kind as u16, header.opcode, "{}", c.file);
            assert_eq!(encode_event(&ev), payload, "{}", c.file);
            continue;
        }
        if header.status != 0 {
            std::str::from_utf8(payload).expect("error detail must be UTF-8");
            continue;
        }
        match (header.opcode, header.flags & FLAG_RESPONSE != 0) {
            (opcode::HELLO, false) => {
                assert_eq!(HelloReq::decode(payload).unwrap().encode(), payload);
            }
            (opcode::HELLO, true) => {
                assert_eq!(HelloResp::decode(payload).unwrap().encode(), payload);
            }
            (opcode::SUBSCRIBE | opcode::UNSUBSCRIBE, _) => {
                assert!(payload.is_empty(), "{}", c.file);
            }
            (opcode::LIST_VOLUMES | opcode::INDEX_STATUS, true) => {
                let v: Vec<VolumeStatusWire> =
                    fmf_proto::messages::decode_json("VolumeStatuses", payload).unwrap();
                assert_eq!(encode_json("VolumeStatuses", &v).unwrap(), payload);
            }
            (opcode::INDEX_START, false) => {
                let v: IndexStartReq =
                    fmf_proto::messages::decode_json("IndexStart", payload).unwrap();
                assert_eq!(encode_json("IndexStart", &v).unwrap(), payload);
            }
            (opcode::QUERY, false) => {
                let (opt, text) = fmf_proto::messages::decode_query_req(payload).unwrap();
                assert_eq!(encode_query_req(opt, text), payload);
            }
            (opcode::QUERY, true) => {
                let (head, trace) = QueryRespHead::decode(payload).unwrap();
                assert_eq!(head.encode_with_trace(trace), payload);
            }
            (opcode::RESULT_PAGE, false) => {
                assert_eq!(ResultPageReq::decode(payload).unwrap().encode(), payload);
            }
            (opcode::RESULT_PAGE, true) => {
                let page = fmf_proto::messages::decode_page(payload).unwrap();
                assert_eq!(
                    encode_page(&page.rows, page.blob).expect("golden page re-encodes"),
                    payload
                );
            }
            (opcode::RESULT_FREE, false) => {
                let id = fmf_proto::messages::decode_result_free(payload).unwrap();
                assert_eq!(encode_result_free(id), payload);
            }
            (opcode::SERVICE_INFO, true) => {
                let v: ServiceInfoResp =
                    fmf_proto::messages::decode_json("ServiceInfo", payload).unwrap();
                assert_eq!(encode_json("ServiceInfo", &v).unwrap(), payload);
            }
            other => panic!("{}: unhandled corpus case {other:?}", c.file),
        }
    }
}

/// `fmf-contract`'s opcode table, read as *text* so the set is exhaustive by
/// construction: a new `pub const … : u16 = …;` shows up here the moment it is
/// declared, which is what makes [`every_opcode_is_captured_or_excluded`] a
/// real gate rather than a hand-maintained list that drifts.
const OPCODE_SOURCE: &str = include_str!("../../fmf-contract/src/opcodes.rs");

/// Opcodes deliberately shipped without a captured frame, each with the reason
/// it earns the exemption. Adding an entry is the explicit alternative to
/// blessing a case — never the quiet default.
const UNCAPTURED_OPCODES: &[(&str, &str)] = &[
    // Request payload is empty and the response is the metrics snapshot, whose
    // authoritative shape is already pinned byte-for-byte by
    // contract/golden/stats_snapshot.json (fmf-core's golden_json test) and
    // re-read from C# by GoldenCorpusTests. A frame case would add only the
    // 16-byte header, a layout every other case already pins.
    (
        "STATS",
        "payload-free request; response shape pinned by stats_snapshot.json",
    ),
    // One-way control frame: zero payload, no response, so the capture would
    // again be header-only. Its behaviour — abort a queued/running query,
    // silent no-op for an unknown id, pre-Hello disconnect, zero-payload cap —
    // is pinned by fmf-service's pipe_loopback suite, where it is observable.
    (
        "QUERY_CANCEL",
        "one-way zero-payload control frame; behaviour pinned by pipe_loopback",
    ),
];

fn declared_opcodes() -> Vec<(&'static str, u16)> {
    OPCODE_SOURCE
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("pub const ")?;
            let (name, rest) = rest.split_once(": u16 = ")?;
            let value = rest.trim().strip_suffix(';')?.trim().parse().ok()?;
            Some((name, value))
        })
        .collect()
}

/// Every declared opcode must own at least one corpus frame, or be named in
/// [`UNCAPTURED_OPCODES`] with a reason. Without this, a protocol bump can add
/// an opcode and stay entirely outside the golden corpus — `manifest.json`
/// only proves manifest/disk symmetry, which a brand new opcode satisfies
/// trivially by contributing nothing to either side.
#[test]
fn every_opcode_is_captured_or_excluded() {
    let declared = declared_opcodes();
    // The parse is text-based, so prove it still sees the table: two anchors
    // cross-checked against the compiled constants, plus the known count.
    for (name, expected) in [
        ("HELLO", opcode::HELLO),
        ("QUERY_CANCEL", opcode::QUERY_CANCEL),
    ] {
        assert_eq!(
            declared.iter().find(|(n, _)| *n == name).map(|&(_, v)| v),
            Some(expected),
            "opcode source parse missed {name} — the `pub const NAME: u16 = N;` \
             shape in fmf-contract/src/opcodes.rs changed and this test went blind"
        );
    }
    assert!(
        declared.len() >= 12,
        "opcode source parse found only {} constants",
        declared.len()
    );

    // Event pushes reuse kinds 1..=6 in the opcode field, so counting them
    // would credit request opcodes they have nothing to do with.
    let captured: std::collections::BTreeSet<u16> = corpus()
        .iter()
        .filter_map(|c| {
            let header_bytes: [u8; HEADER_LEN] = c.bytes[..HEADER_LEN].try_into().unwrap();
            let header = decode_header(&header_bytes).unwrap();
            (header.flags & FLAG_EVENT == 0).then_some(header.opcode)
        })
        .collect();

    for (name, value) in &declared {
        let excused = UNCAPTURED_OPCODES.iter().any(|(n, _)| n == name);
        assert!(
            captured.contains(value) != excused,
            "opcode {name} ({value}): {}",
            if excused {
                "listed in UNCAPTURED_OPCODES but the corpus captures it — drop the stale entry"
            } else {
                "no corpus case. Add one to `corpus()` and re-capture with \
                 `just contract-bless`, or add a reasoned entry to UNCAPTURED_OPCODES. \
                 Note that a new case also needs a handler in the C# GoldenCorpusTests \
                 switch, which fails closed on unknown opcodes"
            }
        );
    }

    for (name, reason) in UNCAPTURED_OPCODES {
        assert!(
            declared.iter().any(|(n, _)| n == name),
            "UNCAPTURED_OPCODES names {name}, which is no longer a declared opcode"
        );
        assert!(!reason.is_empty(), "{name}: an exclusion must state why");
    }
}

/// manifest.json indexes every corpus file with its description; the pin
/// run fails when a case is added or renamed without a bless — and the C#
/// suite walks the same manifest, so neither side can silently skip files.
#[test]
fn manifest_matches_the_corpus() {
    let manifest: Vec<serde_json::Value> = corpus()
        .iter()
        .map(|c| serde_json::json!({ "file": c.file, "desc": c.desc }))
        .collect();
    let doc = serde_json::json!({
        "version": 1,
        "comment": "Captured wire bytes — the executable contract spec. \
                    Re-capture only via FMF_BLESS=1 (ADR-0018).",
        "cases": manifest,
    });
    let bytes = serde_json::to_vec_pretty(&doc).unwrap();
    check_file("manifest.json", &bytes);
}
