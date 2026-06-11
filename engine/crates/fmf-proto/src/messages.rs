//! Payload codecs. Binary payloads are little-endian, padding free,
//! concatenated in documented order; cold-path payloads are UTF-8 JSON with
//! snake_case fields (docs/ARCHITECTURE.md「Pipe プロトコル」§オペコード表
//! — the canonical table). The types themselves come from `fmf_contract`;
//! only the byte logic lives here.

use serde::{Deserialize, Serialize};

pub use fmf_contract::opcodes as opcode;
pub use fmf_contract::pod::{FmfEvent, FmfQueryOptions, FmfRow};

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("payload is {got} bytes, expected {expected} for {what}")]
    Length {
        what: &'static str,
        expected: usize,
        got: usize,
    },
    #[error("payload of {what} is not valid UTF-8")]
    Utf8 { what: &'static str },
    #[error("json {what}: {source}")]
    Json {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn check_len(what: &'static str, b: &[u8], expected: usize) -> Result<(), WireError> {
    if b.len() != expected {
        return Err(WireError::Length {
            what,
            expected,
            got: b.len(),
        });
    }
    Ok(())
}

// ── Hello (op 1, binary) ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloReq {
    pub protocol_version: u32,
}

impl HelloReq {
    pub const LEN: usize = 4;

    pub fn encode(self) -> Vec<u8> {
        self.protocol_version.to_le_bytes().to_vec()
    }

    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        check_len("HelloReq", b, Self::LEN)?;
        Ok(Self {
            protocol_version: u32_at(b, 0),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloResp {
    pub protocol_version: u32,
    pub abi_version: u32,
    pub server_pid: u32,
}

impl HelloResp {
    pub const LEN: usize = 12;

    pub fn encode(self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN);
        v.extend_from_slice(&self.protocol_version.to_le_bytes());
        v.extend_from_slice(&self.abi_version.to_le_bytes());
        v.extend_from_slice(&self.server_pid.to_le_bytes());
        v
    }

    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        check_len("HelloResp", b, Self::LEN)?;
        Ok(Self {
            protocol_version: u32_at(b, 0),
            abi_version: u32_at(b, 4),
            server_pid: u32_at(b, 8),
        })
    }
}

// ── Query (op 7, binary options + UTF-8 text) ───────────────────────────

pub fn encode_query_req(opt: FmfQueryOptions, text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(FmfQueryOptions::LEN + text.len());
    v.extend_from_slice(&opt.sort.to_le_bytes());
    v.extend_from_slice(&opt.desc.to_le_bytes());
    v.extend_from_slice(&opt.case_mode.to_le_bytes());
    v.extend_from_slice(&opt.include_hidden_system.to_le_bytes());
    v.extend_from_slice(text.as_bytes());
    v
}

pub fn decode_query_req(b: &[u8]) -> Result<(FmfQueryOptions, &str), WireError> {
    if b.len() < FmfQueryOptions::LEN {
        return Err(WireError::Length {
            what: "QueryReq",
            expected: FmfQueryOptions::LEN,
            got: b.len(),
        });
    }
    let opt = FmfQueryOptions {
        sort: u32_at(b, 0),
        desc: u32_at(b, 4),
        case_mode: u32_at(b, 8),
        include_hidden_system: u32_at(b, 12),
    };
    let text = std::str::from_utf8(&b[FmfQueryOptions::LEN..])
        .map_err(|_| WireError::Utf8 { what: "QueryReq" })?;
    Ok((opt, text))
}

/// Query response head; the QueryTrace JSON follows it verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryRespHead {
    pub result_id: u64,
    pub count: u64,
}

impl QueryRespHead {
    pub const LEN: usize = 16;

    pub fn encode_with_trace(self, trace_json: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN + trace_json.len());
        v.extend_from_slice(&self.result_id.to_le_bytes());
        v.extend_from_slice(&self.count.to_le_bytes());
        v.extend_from_slice(trace_json);
        v
    }

    pub fn decode(b: &[u8]) -> Result<(Self, &[u8]), WireError> {
        if b.len() < Self::LEN {
            return Err(WireError::Length {
                what: "QueryResp",
                expected: Self::LEN,
                got: b.len(),
            });
        }
        Ok((
            Self {
                result_id: u64_at(b, 0),
                count: u64_at(b, 8),
            },
            &b[Self::LEN..],
        ))
    }
}

// ── ResultPage (op 8, binary) ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultPageReq {
    pub result_id: u64,
    pub offset: u64,
    pub count: u32,
}

impl ResultPageReq {
    pub const LEN: usize = 20;

    pub fn encode(self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN);
        v.extend_from_slice(&self.result_id.to_le_bytes());
        v.extend_from_slice(&self.offset.to_le_bytes());
        v.extend_from_slice(&self.count.to_le_bytes());
        v
    }

    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        check_len("ResultPageReq", b, Self::LEN)?;
        Ok(Self {
            result_id: u64_at(b, 0),
            offset: u64_at(b, 8),
            count: u32_at(b, 16),
        })
    }
}

/// One row on the wire — byte-for-byte the FFI `FmfRow` (48 bytes, no
/// padding; offsets are relative to the string blob start).
fn write_row(v: &mut Vec<u8>, r: &FmfRow) {
    v.extend_from_slice(&r.entry_ref.to_le_bytes());
    v.extend_from_slice(&r.frn.to_le_bytes());
    v.extend_from_slice(&r.size.to_le_bytes());
    v.extend_from_slice(&r.mtime.to_le_bytes());
    v.extend_from_slice(&r.name_off.to_le_bytes());
    v.extend_from_slice(&r.parent_path_off.to_le_bytes());
    v.extend_from_slice(&r.flags.to_le_bytes());
    v.extend_from_slice(&r.name_len.to_le_bytes());
    v.extend_from_slice(&r.parent_path_len.to_le_bytes());
}

fn read_row_at(b: &[u8], off: usize) -> FmfRow {
    FmfRow {
        entry_ref: u64_at(b, off),
        frn: u64_at(b, off + 8),
        size: u64_at(b, off + 16),
        mtime: u64_at(b, off + 24) as i64,
        name_off: u32_at(b, off + 32),
        parent_path_off: u32_at(b, off + 36),
        flags: u32_at(b, off + 40),
        name_len: u16_at(b, off + 44),
        parent_path_len: u16_at(b, off + 46),
    }
}

/// Decoded view of a ResultPage response payload:
/// `{row_count:u32, blob_len:u32}` → rows (48 B × row_count) → blob.
pub struct PageView<'a> {
    pub rows: Vec<FmfRow>,
    pub blob: &'a [u8],
}

pub fn encode_page(rows: &[FmfRow], blob: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + rows.len() * FmfRow::LEN + blob.len());
    v.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    v.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    for r in rows {
        write_row(&mut v, r);
    }
    v.extend_from_slice(blob);
    v
}

pub fn decode_page(b: &[u8]) -> Result<PageView<'_>, WireError> {
    if b.len() < 8 {
        return Err(WireError::Length {
            what: "PageResp",
            expected: 8,
            got: b.len(),
        });
    }
    let row_count = u32_at(b, 0) as usize;
    let blob_len = u32_at(b, 4) as usize;
    let expected = 8 + row_count * FmfRow::LEN + blob_len;
    check_len("PageResp", b, expected)?;
    let rows = (0..row_count)
        .map(|i| read_row_at(b, 8 + i * FmfRow::LEN))
        .collect();
    Ok(PageView {
        rows,
        blob: &b[8 + row_count * FmfRow::LEN..],
    })
}

// ── ResultFree (op 9, binary) ───────────────────────────────────────────

pub fn encode_result_free(result_id: u64) -> Vec<u8> {
    result_id.to_le_bytes().to_vec()
}

pub fn decode_result_free(b: &[u8]) -> Result<u64, WireError> {
    check_len("ResultFree", b, 8)?;
    Ok(u64_at(b, 0))
}

// ── Event push (flags = FLAG_EVENT, opcode = kind 1..=6, binary) ────────

/// Body is the FFI `FmfEvent` POD (32 bytes), serialized explicitly:
/// `{kind:u32, _pad:u32(0), entries:u64, volume:[u8;16]}`.
pub fn encode_event(ev: &FmfEvent) -> Vec<u8> {
    let mut v = Vec::with_capacity(FmfEvent::LEN);
    v.extend_from_slice(&ev.kind.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // _pad
    v.extend_from_slice(&ev.entries.to_le_bytes());
    v.extend_from_slice(&ev.volume);
    v
}

pub fn decode_event(b: &[u8]) -> Result<FmfEvent, WireError> {
    check_len("Event", b, FmfEvent::LEN)?;
    Ok(FmfEvent {
        kind: u32_at(b, 0),
        _pad: 0,
        entries: u64_at(b, 8),
        volume: b[16..32].try_into().unwrap(),
    })
}

// ── JSON messages (op 4/5/6/10/12) ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeStatusWire {
    /// Drive label, e.g. "C:" — the one volume identifier on the wire.
    pub volume: String,
    /// Same values as FFI FmfVolumeStatus.state (0=Scanning 1=Ready
    /// 2=Rescanning 3=Failed).
    pub state: u32,
    pub entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStartReq {
    pub volumes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfoResp {
    pub uptime_ms: u64,
    pub connections: u32,
    pub version: String,
}

pub fn encode_json<T: Serialize>(what: &'static str, v: &T) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(v).map_err(|source| WireError::Json { what, source })
}

pub fn decode_json<'a, T: Deserialize<'a>>(
    what: &'static str,
    b: &'a [u8],
) -> Result<T, WireError> {
    serde_json::from_slice(b).map_err(|source| WireError::Json { what, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip_and_golden_bytes() {
        let req = HelloReq {
            protocol_version: 1,
        };
        assert_eq!(req.encode(), [1, 0, 0, 0]);
        assert_eq!(HelloReq::decode(&req.encode()).unwrap(), req);

        let resp = HelloResp {
            protocol_version: 1,
            abi_version: 1,
            server_pid: 0x0403_0201,
        };
        assert_eq!(resp.encode(), [1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 3, 4]);
        assert_eq!(HelloResp::decode(&resp.encode()).unwrap(), resp);
    }

    #[test]
    fn query_req_roundtrip_and_golden_bytes() {
        let opt = FmfQueryOptions {
            sort: 1,
            desc: 1,
            case_mode: 2,
            include_hidden_system: 0,
        };
        let bytes = encode_query_req(opt, "win");
        assert_eq!(
            bytes,
            [
                1, 0, 0, 0, // sort=Size
                1, 0, 0, 0, // desc
                2, 0, 0, 0, // case=Sensitive
                0, 0, 0, 0, // include_hidden_system
                b'w', b'i', b'n',
            ]
        );
        let (d_opt, d_text) = decode_query_req(&bytes).unwrap();
        assert_eq!(d_opt, opt);
        assert_eq!(d_text, "win");
        // Empty query text is wire-legal (the server rejects it, not the codec).
        let empty_req = encode_query_req(opt, "");
        let (_, empty) = decode_query_req(&empty_req).unwrap();
        assert_eq!(empty, "");
        // Invalid UTF-8 text is a codec error.
        let mut bad = encode_query_req(opt, "");
        bad.push(0xFF);
        assert!(decode_query_req(&bad).is_err());
    }

    #[test]
    fn page_roundtrip_pins_the_48_byte_row() {
        let row = FmfRow {
            entry_ref: 1,
            frn: (1 << 48) | 100,
            size: 1234,
            mtime: -5,
            name_off: 0,
            parent_path_off: 9,
            flags: 1,
            name_len: 9,
            parent_path_len: 3,
        };
        let blob = b"alpha.txtC:\\";
        let bytes = encode_page(&[row], blob);
        assert_eq!(bytes.len(), 8 + FmfRow::LEN + blob.len());
        let v = decode_page(&bytes).unwrap();
        assert_eq!(v.rows, vec![row]);
        assert_eq!(v.blob, blob);

        // Lying header lengths must not panic or over-read.
        let mut lying = bytes.clone();
        lying[0..4].copy_from_slice(&2u32.to_le_bytes()); // row_count=2, but only 1 row present
        assert!(decode_page(&lying).is_err());
    }

    #[test]
    fn event_roundtrip_and_label_semantics() {
        let ev = FmfEvent::new(3, 7, "C:");
        let b = encode_event(&ev);
        assert_eq!(b.len(), FmfEvent::LEN);
        let d = decode_event(&b).unwrap();
        assert_eq!(d, ev);
        assert_eq!(d.volume_str(), "C:");
    }

    #[test]
    fn json_messages_are_snake_case() {
        let v = VolumeStatusWire {
            volume: "C:".into(),
            state: 1,
            entries: 42,
        };
        let json = String::from_utf8(encode_json("v", &vec![v.clone()]).unwrap()).unwrap();
        assert_eq!(json, r#"[{"volume":"C:","state":1,"entries":42}]"#);
        let back: Vec<VolumeStatusWire> = decode_json("v", json.as_bytes()).unwrap();
        assert_eq!(back, vec![v]);

        let info = ServiceInfoResp {
            uptime_ms: 1,
            connections: 2,
            version: "0.1.0".into(),
        };
        let json = String::from_utf8(encode_json("i", &info).unwrap()).unwrap();
        assert!(json.contains("uptime_ms"));
    }
}
