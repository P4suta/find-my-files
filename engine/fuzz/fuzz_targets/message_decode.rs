#![no_main]
//! Fuzz every fmf-proto payload decoder over arbitrary bytes.
//! These run inside the elevated service on client-supplied payloads, so none may panic,
//! over-read, or overflow.
//! `decode_page` is the sharpest target: it derives row/blob lengths from an attacker-controlled header and indexes into the buffer accordingly.

use fmf_proto::messages;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(prefix) = data.get(..messages::HelloReq::LEN) {
        let hello = messages::HelloReq::decode(prefix).expect("exact handshake length");
        assert_eq!(hello.encode(), prefix);
    }
    if let Ok(hello) = messages::HelloResp::decode(data) {
        assert_eq!(hello.encode(), data);
    }
    if let Ok((options, text)) = messages::decode_query_req(data) {
        assert_eq!(messages::encode_query_req(options, text), data);
    }
    let _ = messages::HelloReq::decode(data);
    let _ = messages::HelloResp::decode(data);
    let _ = messages::ResultPageReq::decode(data);
    let _ = messages::decode_result_free(data);
    let _ = messages::decode_event(data);

    let _ = messages::QueryRespHead::decode(data);
    let _ = messages::decode_query_req(data);
    let _ = messages::decode_page(data);

    let _ = messages::decode_json::<messages::IndexStartReq>("IndexStartReq", data);
    let _ = messages::decode_json::<messages::ServiceInfoResp>("ServiceInfoResp", data);
    let _ = messages::decode_json::<Vec<messages::VolumeStatusWire>>("VolumeStatus", data);
});
