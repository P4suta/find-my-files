use crate::pod::FrameHeader;
use crate::volume::{encode_label_bytes, label_prefix};

const MAX_LABEL_INPUT_BYTES: usize = 32;
const LABEL_BYTES: usize = 16;
const U32_BYTES: usize = size_of::<u32>();
const U16_BYTES: usize = size_of::<u16>();
const OPCODE_OFFSET: usize = U32_BYTES;
const FLAGS_OFFSET: usize = OPCODE_OFFSET + U16_BYTES;
const REQUEST_ID_OFFSET: usize = FLAGS_OFFSET + U16_BYTES;
const STATUS_OFFSET: usize = REQUEST_ID_OFFSET + U32_BYTES;

fn arbitrary_header() -> FrameHeader {
    FrameHeader {
        len: kani::any(),
        opcode: kani::any(),
        flags: kani::any(),
        request_id: kani::any(),
        status: kani::any(),
    }
}

#[kani::proof]
#[kani::unwind(17)]
fn frame_header_fields_roundtrip() {
    let header = arbitrary_header();
    assert_eq!(FrameHeader::from_bytes(&header.to_bytes()), header);
    kani::cover!(header.status < 0);
    kani::cover!(header.len == u32::MAX);
}

#[kani::proof]
#[kani::unwind(17)]
fn frame_header_bytes_roundtrip() {
    let bytes: [u8; FrameHeader::LEN] = kani::any();
    assert_eq!(FrameHeader::from_bytes(&bytes).to_bytes(), bytes);
    kani::cover!(bytes == [0; FrameHeader::LEN]);
    kani::cover!(bytes == [u8::MAX; FrameHeader::LEN]);
}

#[kani::proof]
#[kani::unwind(17)]
fn frame_header_matches_little_endian_layout() {
    let header = arbitrary_header();
    let bytes = header.to_bytes();
    for offset in 0..U32_BYTES {
        let shift = offset * u8::BITS as usize;
        assert_eq!(bytes[offset], (header.len >> shift) as u8);
        assert_eq!(
            bytes[REQUEST_ID_OFFSET + offset],
            (header.request_id >> shift) as u8
        );
        assert_eq!(
            bytes[STATUS_OFFSET + offset],
            ((header.status as u32) >> shift) as u8
        );
    }
    for offset in 0..U16_BYTES {
        let shift = offset * u8::BITS as usize;
        assert_eq!(
            bytes[OPCODE_OFFSET + offset],
            (header.opcode >> shift) as u8
        );
        assert_eq!(bytes[FLAGS_OFFSET + offset], (header.flags >> shift) as u8);
    }
    kani::cover!(header.flags == u16::MAX);
    kani::cover!(header.status == i32::MIN);
}

#[kani::proof]
#[kani::unwind(33)]
fn encoded_bytes_preserve_prefix_and_zero_padding() {
    let bytes: [u8; MAX_LABEL_INPUT_BYTES] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= bytes.len());
    let encoded = encode_label_bytes(&bytes[..len]);
    let prefix_len = len.min(encoded.len() - 1);
    assert_eq!(&encoded[..prefix_len], &bytes[..prefix_len]);
    assert!(encoded[prefix_len..].iter().all(|byte| *byte == 0));
    kani::cover!(len == 0);
    kani::cover!(len == bytes.len());
}

#[kani::proof]
#[kani::unwind(17)]
fn encoded_labels_always_terminate() {
    let bytes: [u8; MAX_LABEL_INPUT_BYTES] = kani::any();
    let encoded = encode_label_bytes(&bytes);
    assert_eq!(encoded[LABEL_BYTES - 1], 0);
    kani::cover!(bytes[0] == 0);
    kani::cover!(bytes[0] == u8::MAX);
}

#[kani::proof]
#[kani::unwind(33)]
fn label_prefix_stops_at_first_zero() {
    let bytes: [u8; LABEL_BYTES] = kani::any();
    let label = label_prefix(&bytes);
    assert!(label.len() <= bytes.len());
    assert_eq!(label, &bytes[..label.len()]);
    assert!(!label.contains(&0));
    if let Some(first_zero) = bytes.iter().position(|byte| *byte == 0) {
        assert_eq!(label.len(), first_zero);
    } else {
        assert_eq!(label.len(), bytes.len());
    }
    kani::cover!(label.is_empty());
    kani::cover!(label.len() == bytes.len());
}
