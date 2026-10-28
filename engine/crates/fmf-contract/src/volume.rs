//! Fixed-width volume-label byte conversion.

/// Copies the UTF-8 prefix into the fixed field and reserves its last byte for NUL.
/// Truncation can split a Unicode scalar; [`decode_label`] then returns an empty string.
#[must_use]
pub fn encode_label(label: &str) -> [u8; 16] {
    encode_label_bytes(label.as_bytes())
}

pub(crate) fn encode_label_bytes(bytes: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let n = bytes.len().min(out.len() - 1);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Reads up to the first NUL and returns an empty string when that prefix is invalid UTF-8.
#[must_use]
pub fn decode_label(bytes: &[u8; 16]) -> &str {
    core::str::from_utf8(label_prefix(bytes)).unwrap_or("")
}

pub(crate) fn label_prefix(bytes: &[u8; 16]) -> &[u8] {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_caps_and_pads() {
        assert_eq!(decode_label(&encode_label("C:")), "C:");
        assert_eq!(encode_label("C:")[2..], [0u8; 14]);
        assert_eq!(decode_label(&encode_label("")), "");
        let long = encode_label("0123456789abcdefgh");
        assert_eq!(long[15], 0);
        assert_eq!(decode_label(&long), "0123456789abcde");
    }

    #[test]
    fn decoding_rejects_invalid_utf8_and_stops_before_trailing_bytes() {
        let mut bytes = [0u8; 16];
        bytes[0] = 0xff;
        assert_eq!(decode_label(&bytes), "");
        bytes[..3].copy_from_slice(b"C:\0");
        bytes[3..].fill(0xff);
        assert_eq!(decode_label(&bytes), "C:");
    }

    #[test]
    fn every_drive_letter_roundtrips() {
        for letter in b'A'..=b'Z' {
            let bytes = [letter, b':'];
            let label = core::str::from_utf8(&bytes).expect("ASCII drive label");
            assert_eq!(decode_label(&encode_label(label)), label);
        }
    }

    #[test]
    fn truncation_keeps_bytes_and_rejects_a_split_unicode_scalar() {
        let label = "0123456789abcdé";
        let encoded = encode_label(label);
        assert_eq!(&encoded[..15], &label.as_bytes()[..15]);
        assert_eq!(encoded[15], 0);
        assert_eq!(decode_label(&encoded), "");
        assert_eq!(decode_label(&encode_label("é")), "é");
    }
}
