#![no_main]
//! Fuzz the named-pipe frame reader — the first parser the elevated fmf-service
//! runs on bytes from a (possibly hostile) client. It must never panic: a
//! malformed or oversized header is a clean `Err` that tears down the
//! connection, never a crash in a privileged process.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Header decode over the fixed 16-byte prefix, when present (exercises the
    // MAX_PAYLOAD_LEN cap directly, independent of stream length).
    if data.len() >= fmf_proto::frame::HEADER_LEN {
        let mut hb = [0u8; fmf_proto::frame::HEADER_LEN];
        hb.copy_from_slice(&data[..fmf_proto::frame::HEADER_LEN]);
        let _ = fmf_proto::frame::decode_header(&hb);
    }

    // Full frame read over the stream: header + length-prefixed payload. A
    // truncated stream (header announces more than `data` holds) must error,
    // not over-allocate or hang.
    let mut cur = Cursor::new(data);
    let _ = fmf_proto::frame::read_frame(&mut cur);
});
