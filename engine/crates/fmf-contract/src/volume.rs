//! Volume label ⇄ 16-byte field — the one implementation of the contract's
//! "UTF-8 drive label, zero-padded, not a GUID" rule (used by `FmfEvent`,
//! `FmfVolumeStatus` and the pipe event body).

/// Zero-padded UTF-8, capped at 15 bytes (the last byte stays NUL so the C
/// side can treat the field as NUL-terminated).
pub fn encode_label(label: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let bytes = label.as_bytes();
    let n = bytes.len().min(15);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Reads up to the first NUL; non-UTF-8 content decodes as "" (defensive —
/// well-formed peers never produce it).
pub fn decode_label(bytes: &[u8; 16]) -> &str {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(16);
    core::str::from_utf8(&bytes[..len]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_caps_and_pads() {
        assert_eq!(decode_label(&encode_label("C:")), "C:");
        assert_eq!(encode_label("C:")[2..], [0u8; 14]);
        assert_eq!(decode_label(&encode_label("")), "");
        // 16+ bytes cap at 15, preserving NUL termination.
        let long = encode_label("0123456789abcdefgh");
        assert_eq!(long[15], 0);
        assert_eq!(decode_label(&long), "0123456789abcde");
    }
}
