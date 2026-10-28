#![no_main]
//! Fuzz the named-pipe frame reader — the first parser the elevated fmf-service runs on bytes from a (possibly hostile) client.
//! It must never panic: a malformed or oversized header is a clean `Err` that tears down the connection, never a crash in a privileged process.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    if data.len() >= fmf_proto::frame::HEADER_LEN {
        let mut hb = [0u8; fmf_proto::frame::HEADER_LEN];
        hb.copy_from_slice(&data[..fmf_proto::frame::HEADER_LEN]);
        if let Ok(header) = fmf_proto::frame::decode_header(&hb) {
            assert_eq!(header.to_bytes(), hb);
        }
    }

    let mut cur = Cursor::new(data);
    if let Ok((header, body)) = fmf_proto::frame::read_frame(&mut cur) {
        let mut encoded = Vec::new();
        fmf_proto::frame::write_frame(&mut encoded, header, &body).expect("decoded frame encodes");
        assert_eq!(&encoded, &data[..encoded.len()]);
    }
});
