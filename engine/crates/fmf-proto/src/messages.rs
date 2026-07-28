//! Payload codecs.
//!
//! Binary payloads are little-endian, padding free, and concatenated in the
//! order each codec below documents; cold-path payloads are UTF-8 JSON with
//! `snake_case` field names (serde's default — do not add renames). The types
//! themselves come from `fmf_contract` (see [`opcode`] for which payload
//! belongs to which operation); only the byte logic lives here.
//!
//! A volume is identified everywhere by its drive-label string (`"C:"`),
//! never a GUID.

use serde::{Deserialize, Serialize};

pub use fmf_contract::opcodes as opcode;
use fmf_contract::pod::row_flags;
pub use fmf_contract::pod::{FmfEvent, FmfQueryOptions, FmfRow};

/// Why a payload failed to decode (or encode, for JSON).
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// Payload byte length did not match the expected fixed/derived size.
    #[error("payload is {got} bytes, expected {expected} for {what}")]
    Length {
        /// Message kind that failed (for diagnostics).
        what: &'static str,
        /// Expected payload length in bytes.
        expected: usize,
        /// Actual payload length in bytes received.
        got: usize,
    },
    /// Trailing text payload was not valid UTF-8.
    #[error("payload of {what} is not valid UTF-8")]
    Utf8 {
        /// Message kind that failed (for diagnostics).
        what: &'static str,
    },
    /// JSON (de)serialization failed for a cold-path message.
    #[error("json {what}: {source}")]
    Json {
        /// Message kind that failed (for diagnostics).
        what: &'static str,
        /// Underlying `serde_json` error.
        #[source]
        source: serde_json::Error,
    },
    /// A bounded payload component exceeded the machine-readable contract.
    #[error("{what} is {got}, maximum is {maximum}")]
    Limit {
        /// Component that exceeded its limit.
        what: &'static str,
        /// Contract maximum.
        maximum: usize,
        /// Observed value.
        got: usize,
    },
    /// A row's blob window or reserved field violated the page invariant.
    #[error("invalid result-page row: {0}")]
    InvalidPageRow(&'static str),
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

fn bytes16_at(b: &[u8], off: usize) -> [u8; 16] {
    let mut a = [0u8; 16];
    a.copy_from_slice(&b[off..off + 16]);
    a
}

const fn check_len(what: &'static str, b: &[u8], expected: usize) -> Result<(), WireError> {
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

/// Client handshake request (op 1): announces the protocol version it speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloReq {
    /// Pipe protocol version the client expects to speak.
    pub protocol_version: u32,
}

impl HelloReq {
    /// Encoded payload length in bytes.
    pub const LEN: usize = 4;

    /// Encode this request into its little-endian wire bytes.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        self.protocol_version.to_le_bytes().to_vec()
    }

    /// # Errors
    ///
    /// Returns [`WireError::Length`] if `b` is not exactly [`Self::LEN`] bytes.
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        check_len("HelloReq", b, Self::LEN)?;
        Ok(Self {
            protocol_version: u32_at(b, 0),
        })
    }
}

/// Server handshake response (op 1): the versions and PID the server reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloResp {
    /// Pipe protocol version the server speaks.
    pub protocol_version: u32,
    /// FFI ABI version the server's core was built against.
    pub abi_version: u32,
    /// OS process id of the serving fmf-service.
    pub server_pid: u32,
}

impl HelloResp {
    /// Encoded payload length in bytes.
    pub const LEN: usize = 12;

    /// Encode this response into its little-endian wire bytes.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN);
        v.extend_from_slice(&self.protocol_version.to_le_bytes());
        v.extend_from_slice(&self.abi_version.to_le_bytes());
        v.extend_from_slice(&self.server_pid.to_le_bytes());
        v
    }

    /// # Errors
    ///
    /// Returns [`WireError::Length`] if `b` is not exactly [`Self::LEN`] bytes.
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

/// Encode a query request (op 7): the fixed options header followed by the
/// UTF-8 query text.
#[must_use]
pub fn encode_query_req(opt: FmfQueryOptions, text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(FmfQueryOptions::LEN + text.len());
    v.extend_from_slice(&opt.sort.to_le_bytes());
    v.extend_from_slice(&opt.desc.to_le_bytes());
    v.extend_from_slice(&opt.case_mode.to_le_bytes());
    v.extend_from_slice(&opt.include_hidden_system.to_le_bytes());
    v.extend_from_slice(&opt.regex_mode.to_le_bytes());
    v.extend_from_slice(&opt._reserved.to_le_bytes());
    v.extend_from_slice(&opt.presentation_basis.to_le_bytes());
    v.extend_from_slice(text.as_bytes());
    v
}

/// # Errors
///
/// Returns [`WireError::Length`] if `b` is shorter than the fixed options
/// header, or [`WireError::Utf8`] if the trailing query text is not valid
/// UTF-8.
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
        regex_mode: u32_at(b, 16),
        _reserved: u32_at(b, 20),
        presentation_basis: u64_at(b, 24),
    };
    let text = std::str::from_utf8(&b[FmfQueryOptions::LEN..])
        .map_err(|_| WireError::Utf8 { what: "QueryReq" })?;
    Ok((opt, text))
}

/// Query response head; the `QueryTrace` JSON follows it verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryRespHead {
    /// Server-assigned handle for paging this result set.
    pub result_id: u64,
    /// Total number of matching rows in the result set.
    pub count: u64,
}

impl QueryRespHead {
    /// Encoded head length in bytes (trace JSON follows separately).
    pub const LEN: usize = 16;

    /// Encode the head, then append the `QueryTrace` JSON verbatim.
    #[must_use]
    pub fn encode_with_trace(self, trace_json: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN + trace_json.len());
        v.extend_from_slice(&self.result_id.to_le_bytes());
        v.extend_from_slice(&self.count.to_le_bytes());
        v.extend_from_slice(trace_json);
        v
    }

    /// Begin a response buffer with the 16-byte head written and `trace_cap`
    /// bytes reserved, so the caller appends the `QueryTrace` JSON directly
    /// into it (e.g. `serde_json::to_writer`) — one allocation, no intermediate
    /// trace Vec + copy. The head layout stays defined here, not in the
    /// service. On a serialize failure the caller truncates back to
    /// [`Self::LEN`] for the empty-trace degrade path.
    #[must_use]
    pub fn begin_response(self, trace_cap: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN + trace_cap);
        v.extend_from_slice(&self.result_id.to_le_bytes());
        v.extend_from_slice(&self.count.to_le_bytes());
        v
    }

    /// # Errors
    ///
    /// Returns [`WireError::Length`] if `b` is shorter than [`Self::LEN`].
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

/// Request for a page of rows from a prior result set (op 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultPageReq {
    /// Handle of the result set to page, from `QueryRespHead::result_id`.
    pub result_id: u64,
    /// Index of the first row to return (0-based).
    pub offset: u64,
    /// Maximum number of rows to return in this page.
    pub count: u32,
}

impl ResultPageReq {
    /// Encoded payload length in bytes.
    pub const LEN: usize = 20;

    /// Encode this request into its little-endian wire bytes.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::LEN);
        v.extend_from_slice(&self.result_id.to_le_bytes());
        v.extend_from_slice(&self.offset.to_le_bytes());
        v.extend_from_slice(&self.count.to_le_bytes());
        v
    }

    /// # Errors
    ///
    /// Returns [`WireError::Length`] if `b` is not exactly [`Self::LEN`] bytes.
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        check_len("ResultPageReq", b, Self::LEN)?;
        Ok(Self {
            result_id: u64_at(b, 0),
            offset: u64_at(b, 8),
            count: u32_at(b, 16),
        })
    }
}

/// One row on the wire — byte-for-byte the FFI `FmfRow` (56 bytes, no
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
    v.extend_from_slice(&r._reserved.to_le_bytes());
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
        name_len: u32_at(b, off + 44),
        parent_path_len: u32_at(b, off + 48),
        _reserved: u32_at(b, off + 52),
    }
}

/// Decoded view of a `ResultPage` response payload:
/// `{row_count:u32, blob_len:u32}` → rows (56 B × `row_count`) → blob.
pub struct PageView<'a> {
    /// Decoded rows; string fields point into `blob` by offset.
    pub rows: Vec<FmfRow>,
    /// Packed UTF-8 string blob the rows' name/parent offsets index into.
    pub blob: &'a [u8],
}

/// Encode a result page (op 8): a `{row_count, blob_len}` header, then the
/// fixed-size rows, then the string blob.
///
/// # Errors
///
/// Returns [`WireError::Limit`] when the page exceeds a contract bound, or
/// [`WireError::InvalidPageRow`] when a row points outside `blob` or has a
/// non-zero reserved field.
pub fn encode_page(rows: &[FmfRow], blob: &[u8]) -> Result<Vec<u8>, WireError> {
    validate_page(rows, blob)?;
    let encoded_len = page_encoded_len(rows.len(), blob.len())?;
    let row_count = u32::try_from(rows.len()).map_err(|_| WireError::Limit {
        what: "result-page row count",
        maximum: u32::MAX as usize,
        got: rows.len(),
    })?;
    let blob_len = u32::try_from(blob.len()).map_err(|_| WireError::Limit {
        what: "result-page blob length",
        maximum: u32::MAX as usize,
        got: blob.len(),
    })?;
    let mut v = Vec::with_capacity(encoded_len);
    v.extend_from_slice(&row_count.to_le_bytes());
    v.extend_from_slice(&blob_len.to_le_bytes());
    for r in rows {
        write_row(&mut v, r);
    }
    v.extend_from_slice(blob);
    Ok(v)
}

/// # Errors
///
/// Returns [`WireError::Length`] if `b` is shorter than the 8-byte count
/// header or if its `row_count`/`blob_len` do not match the payload length.
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
    if row_count > fmf_contract::limits::MAX_PAGE_ROWS as usize {
        return Err(WireError::Limit {
            what: "result-page row count",
            maximum: fmf_contract::limits::MAX_PAGE_ROWS as usize,
            got: row_count,
        });
    }
    let expected = page_encoded_len(row_count, blob_len)?;
    check_len("PageResp", b, expected)?;
    let rows = (0..row_count)
        .map(|i| read_row_at(b, 8 + i * FmfRow::LEN))
        .collect::<Vec<_>>();
    let blob = &b[8 + row_count * FmfRow::LEN..];
    validate_page(&rows, blob)?;
    Ok(PageView { rows, blob })
}

fn page_encoded_len(row_count: usize, blob_len: usize) -> Result<usize, WireError> {
    let encoded_len = row_count
        .checked_mul(FmfRow::LEN)
        .and_then(|rows_len| rows_len.checked_add(blob_len))
        .and_then(|body_len| body_len.checked_add(8))
        .ok_or(WireError::Limit {
            what: "result-page encoded length",
            maximum: fmf_contract::limits::MAX_PAYLOAD_LEN as usize,
            got: usize::MAX,
        })?;
    if encoded_len > fmf_contract::limits::MAX_PAYLOAD_LEN as usize {
        return Err(WireError::Limit {
            what: "result-page encoded length",
            maximum: fmf_contract::limits::MAX_PAYLOAD_LEN as usize,
            got: encoded_len,
        });
    }
    Ok(encoded_len)
}

fn validate_page(rows: &[FmfRow], blob: &[u8]) -> Result<(), WireError> {
    if rows.len() > fmf_contract::limits::MAX_PAGE_ROWS as usize {
        return Err(WireError::Limit {
            what: "result-page row count",
            maximum: fmf_contract::limits::MAX_PAGE_ROWS as usize,
            got: rows.len(),
        });
    }
    for row in rows {
        if row.flags & !row_flags::KNOWN_MASK != 0 {
            return Err(WireError::InvalidPageRow(
                "unknown row flag bits must be zero",
            ));
        }
        if row._reserved != 0 {
            return Err(WireError::InvalidPageRow("reserved field must be zero"));
        }
        for (offset, len, field) in [
            (row.name_off, row.name_len, "name window exceeds the blob"),
            (
                row.parent_path_off,
                row.parent_path_len,
                "parent-path window exceeds the blob",
            ),
        ] {
            let end = u64::from(offset) + u64::from(len);
            if end > blob.len() as u64 {
                return Err(WireError::InvalidPageRow(field));
            }
        }
    }
    Ok(())
}

// ── ResultFree (op 9, binary) ───────────────────────────────────────────

/// Encode a result-free request (op 9): releases the result set `result_id`.
#[must_use]
pub fn encode_result_free(result_id: u64) -> Vec<u8> {
    result_id.to_le_bytes().to_vec()
}

/// # Errors
///
/// Returns [`WireError::Length`] if `b` is not exactly 8 bytes.
pub fn decode_result_free(b: &[u8]) -> Result<u64, WireError> {
    check_len("ResultFree", b, 8)?;
    Ok(u64_at(b, 0))
}

// ── Event push (flags = FLAG_EVENT, opcode = kind 1..=6, binary) ────────

/// Body is the FFI `FmfEvent` POD (32 bytes), serialized explicitly:
/// `{kind:u32, _pad:u32(0), entries:u64, volume:[u8;16]}`.
#[must_use]
pub fn encode_event(ev: &FmfEvent) -> Vec<u8> {
    let mut v = Vec::with_capacity(FmfEvent::LEN);
    v.extend_from_slice(&ev.kind.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // _pad
    v.extend_from_slice(&ev.entries.to_le_bytes());
    v.extend_from_slice(&ev.volume);
    v
}

/// # Errors
///
/// Returns [`WireError::Length`] if `b` is not exactly [`FmfEvent::LEN`] bytes.
pub fn decode_event(b: &[u8]) -> Result<FmfEvent, WireError> {
    check_len("Event", b, FmfEvent::LEN)?;
    Ok(FmfEvent {
        kind: u32_at(b, 0),
        _pad: 0,
        entries: u64_at(b, 8),
        volume: bytes16_at(b, 16),
    })
}

// ── JSON messages (op 4/5/6/10/12) ──────────────────────────────────────

/// Per-volume status as carried in the JSON volume-status message (op 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeStatusWire {
    /// Drive label, e.g. "C:" — the one volume identifier on the wire.
    pub volume: String,
    /// Same values as FFI FmfVolumeStatus.state (0=Scanning 1=Ready
    /// 2=Rescanning 3=Failed).
    pub state: u32,
    /// Number of indexed file entries on this volume.
    pub entries: u64,
}

/// Request to begin indexing a set of volumes (op 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexStartReq {
    /// Drive labels to index, e.g. `["C:", "D:"]`.
    pub volumes: Vec<String>,
}

/// Service self-report returned by the info message (op 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceInfoResp {
    /// Service uptime in milliseconds.
    pub uptime_ms: u64,
    /// Number of currently connected pipe clients.
    pub connections: u32,
    /// Service version string, e.g. "0.1.0".
    pub version: String,
}

/// # Errors
///
/// Returns [`WireError::Json`] if `serde_json` fails to serialize `v`.
pub fn encode_json<T: Serialize>(what: &'static str, v: &T) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(v).map_err(|source| WireError::Json { what, source })
}

/// # Errors
///
/// Returns [`WireError::Json`] if `b` is not valid JSON for `T`.
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
            regex_mode: 3, // whole-query regex (bit0) over the full path (bit1)
            _reserved: 0,
            presentation_basis: 0x0102_0304_0506_0708,
        };
        let bytes = encode_query_req(opt, "win");
        assert_eq!(
            bytes,
            [
                1, 0, 0, 0, // sort=Size
                1, 0, 0, 0, // desc
                2, 0, 0, 0, // case=Sensitive
                0, 0, 0, 0, // include_hidden_system
                3, 0, 0, 0, // regex_mode = whole(bit0) | path(bit1)
                0, 0, 0, 0, // reserved
                8, 7, 6, 5, 4, 3, 2, 1, // presentation basis
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
    fn protocol_json_dtos_reject_unknown_fields() {
        assert!(
            decode_json::<VolumeStatusWire>(
                "VolumeStatus",
                br#"{"volume":"C:","state":1,"entries":2,"typo":true}"#
            )
            .is_err()
        );
        assert!(
            decode_json::<IndexStartReq>("IndexStart", br#"{"volumes":["C:"],"typo":true}"#)
                .is_err()
        );
        assert!(
            decode_json::<ServiceInfoResp>(
                "ServiceInfo",
                br#"{"uptime_ms":1,"connections":1,"version":"x","typo":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn page_roundtrip_pins_the_56_byte_row() {
        let row = FmfRow {
            entry_ref: 1,
            frn: (1 << 48) | 0x64,
            size: 1234,
            mtime: -5,
            name_off: 0,
            parent_path_off: 9,
            flags: 1,
            name_len: 9,
            parent_path_len: 3,
            _reserved: 0,
        };
        let blob = b"alpha.txtC:\\";
        let bytes = encode_page(&[row], blob).unwrap();
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
    fn page_preserves_lengths_beyond_u16_and_rejects_invalid_windows() {
        let parent = vec![b'x'; u16::MAX as usize + 1];
        let row = FmfRow {
            entry_ref: 1,
            frn: 1,
            size: 0,
            mtime: 0,
            name_off: 0,
            parent_path_off: 1,
            flags: 0,
            name_len: 1,
            parent_path_len: parent.len() as u32,
            _reserved: 0,
        };
        let mut blob = vec![b'n'];
        blob.extend_from_slice(&parent);

        let bytes = encode_page(&[row], &blob).unwrap();
        let decoded = decode_page(&bytes).unwrap();
        assert_eq!(decoded.rows[0].parent_path_len, 65_536);
        assert_eq!(decoded.blob, blob);

        let mut invalid = row;
        invalid.parent_path_len += 1;
        assert!(matches!(
            encode_page(&[invalid], &blob),
            Err(WireError::InvalidPageRow(_))
        ));
    }

    #[test]
    fn page_rejects_unknown_row_flags_on_encode_and_decode() {
        let row = FmfRow {
            entry_ref: 1,
            frn: 1,
            size: 0,
            mtime: 0,
            name_off: 0,
            parent_path_off: 0,
            flags: row_flags::KNOWN_MASK << 1,
            name_len: 0,
            parent_path_len: 0,
            _reserved: 0,
        };
        assert!(matches!(
            encode_page(&[row], &[]),
            Err(WireError::InvalidPageRow(_))
        ));

        let mut bytes = encode_page(&[FmfRow { flags: 0, ..row }], &[]).unwrap();
        bytes[8 + 40..8 + 44].copy_from_slice(&row.flags.to_le_bytes());
        assert!(matches!(
            decode_page(&bytes),
            Err(WireError::InvalidPageRow(_))
        ));
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
