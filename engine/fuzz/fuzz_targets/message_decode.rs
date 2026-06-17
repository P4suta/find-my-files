#![no_main]
//! Fuzz every fmf-proto payload decoder over arbitrary bytes. These run inside
//! the elevated service on client-supplied payloads, so none may panic,
//! over-read, or overflow. `decode_page` is the sharpest target: it derives
//! row/blob lengths from an attacker-controlled header and indexes into the
//! buffer accordingly.

use fmf_proto::messages;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fixed-length binary decoders: must reject any length but their own.
    let _ = messages::HelloReq::decode(data);
    let _ = messages::HelloResp::decode(data);
    let _ = messages::ResultPageReq::decode(data);
    let _ = messages::decode_result_free(data);
    let _ = messages::decode_event(data);

    // Variable-length binary decoders: header fields drive slicing/UTF-8.
    let _ = messages::QueryRespHead::decode(data);
    let _ = messages::decode_query_req(data);
    let _ = messages::decode_page(data);

    // JSON cold-path messages: serde_json over untrusted text into each
    // concrete wire type.
    let _ = messages::decode_json::<messages::IndexStartReq>("IndexStartReq", data);
    let _ = messages::decode_json::<messages::ServiceInfoResp>("ServiceInfoResp", data);
    let _ = messages::decode_json::<Vec<messages::VolumeStatusWire>>("VolumeStatus", data);
});
