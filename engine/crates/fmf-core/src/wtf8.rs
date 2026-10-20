//! WTF-8 encoding and deterministic storage folding.

use alloc::vec::Vec;

/// Maximum WTF-8 bytes needed for one input UTF-16 code unit.
pub const MAX_BYTES_PER_UTF16_UNIT: usize = 3;

const ASCII_LIMIT: u32 = 0x80;
const ASCII_LAST: u8 = 0x7F;
const TWO_BYTE_LIMIT: u32 = 0x800;
const BMP_LIMIT: u32 = 0x1_0000;
const MAX_CODE_POINT: u32 = 0x10_FFFF;
const CONTINUATION_TAG: u8 = 0x80;
const TWO_BYTE_TAG: u8 = 0xC0;
const THREE_BYTE_TAG: u8 = 0xE0;
const FOUR_BYTE_TAG: u8 = 0xF0;
const CONTINUATION_MASK: u32 = 0x3F;
const CONTINUATION_BITS: u32 = 6;
const THREE_BYTE_SHIFT: u32 = 12;
const FOUR_BYTE_SHIFT: u32 = 18;
const HIGH_SURROGATE_START: u16 = 0xD800;
const HIGH_SURROGATE_END: u16 = 0xDBFF;
const LOW_SURROGATE_START: u16 = 0xDC00;
const LOW_SURROGATE_END: u16 = 0xDFFF;
const SURROGATE_BITS: u32 = 10;
const SURROGATE_MASK: u32 = 0x3FF;
const SURROGATE_PAIR_UNITS: usize = 2;
const UTF16_UNIT_BYTES: usize = size_of::<u16>();
const TWO_BYTE_WIDTH: usize = 2;
const THREE_BYTE_WIDTH: usize = 3;
const FOUR_BYTE_WIDTH: usize = 4;

/// Append the WTF-8 encoding of a single code point (may be a lone surrogate).
#[inline]
fn push_code_point(cp: u32, out: &mut Vec<u8>) {
    if cp < ASCII_LIMIT {
        out.push(cp as u8);
    } else if cp < TWO_BYTE_LIMIT {
        out.push(TWO_BYTE_TAG + (cp >> CONTINUATION_BITS) as u8);
        out.push(CONTINUATION_TAG + (cp & CONTINUATION_MASK) as u8);
    } else if cp < BMP_LIMIT {
        out.push(THREE_BYTE_TAG + (cp >> THREE_BYTE_SHIFT) as u8);
        out.push(CONTINUATION_TAG + ((cp >> CONTINUATION_BITS) & CONTINUATION_MASK) as u8);
        out.push(CONTINUATION_TAG + (cp & CONTINUATION_MASK) as u8);
    } else {
        out.push(FOUR_BYTE_TAG + (cp >> FOUR_BYTE_SHIFT) as u8);
        out.push(CONTINUATION_TAG + ((cp >> THREE_BYTE_SHIFT) & CONTINUATION_MASK) as u8);
        out.push(CONTINUATION_TAG + ((cp >> CONTINUATION_BITS) & CONTINUATION_MASK) as u8);
        out.push(CONTINUATION_TAG + (cp & CONTINUATION_MASK) as u8);
    }
}

#[inline]
const fn utf8_len(cp: u32) -> usize {
    match cp {
        0..ASCII_LIMIT => 1,
        ASCII_LIMIT..TWO_BYTE_LIMIT => TWO_BYTE_WIDTH,
        TWO_BYTE_LIMIT..BMP_LIMIT => THREE_BYTE_WIDTH,
        _ => FOUR_BYTE_WIDTH,
    }
}

/// Lowercase `c` only if the result is a single char with the same encoded length; otherwise return `c` unchanged.
#[inline]
#[must_use]
pub fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) if utf8_len(l as u32) == utf8_len(c as u32) => l,
        _ => c,
    }
}

/// Decode UTF-16 (with possible unpaired surrogates) into code points,
/// invoking `f(cp)` for each.
#[inline]
fn for_each_code_point(mut units: &[u16], mut f: impl FnMut(u32)) {
    while let Some((&u, tail)) = units.split_first() {
        units = tail;
        let cp: u32 = if (HIGH_SURROGATE_START..=HIGH_SURROGATE_END).contains(&u)
            && units
                .first()
                .is_some_and(|low| (LOW_SURROGATE_START..=LOW_SURROGATE_END).contains(low))
        {
            let hi = u32::from(u - HIGH_SURROGATE_START) << SURROGATE_BITS;
            let lo = u32::from(units[0] - LOW_SURROGATE_START);
            units = &units[1..];
            BMP_LIMIT + hi + lo
        } else {
            u as u32
        };
        f(cp);
    }
}

/// Append the folded WTF-8 of one code point to `lower_out`: lowercase a real scalar with the length-preserving rule (ADR-0004), mirror a lone surrogate unchanged.
/// The single fold-side definition shared by the two encoders.
#[inline]
fn push_folded_cp(cp: u32, lower_out: &mut Vec<u8>) {
    match char::from_u32(cp) {
        Some(c) => push_code_point(fold_char(c) as u32, lower_out),
        None => push_code_point(cp, lower_out),
    }
}

#[inline]
fn push_pair_cp(cp: u32, name_out: &mut Vec<u8>, lower_out: &mut Vec<u8>) {
    if cp < ASCII_LIMIT {
        let byte = cp as u8;
        name_out.push(byte);
        lower_out.push(byte.to_ascii_lowercase());
    } else {
        push_code_point(cp, name_out);
        push_folded_cp(cp, lower_out);
    }
}

/// Decode UTF-16 (with possible unpaired surrogates) and append both the WTF-8 original and its folded form.
/// The two outputs always grow by the same number of bytes.
pub fn push_wtf8_pair(units: &[u16], name_out: &mut Vec<u8>, lower_out: &mut Vec<u8>) {
    for_each_code_point(units, |cp| push_pair_cp(cp, name_out, lower_out));
}

/// Encode a checked UTF-16LE byte slice without first materializing `u16`s.
///
/// The NTFS parser hands the initial-scan hot path a view into the record.
/// Rejecting odd byte counts before writing keeps malformed callers atomic;
/// valid NTFS `$FILE_NAME` payloads therefore incur no per-name scratch copy.
pub fn push_wtf8le_pair(bytes: &[u8], name_out: &mut Vec<u8>, lower_out: &mut Vec<u8>) -> bool {
    if !bytes.len().is_multiple_of(UTF16_UNIT_BYTES) {
        return false;
    }
    let mut bytes = bytes;
    while !bytes.is_empty() {
        let unit = u16::from_le_bytes([bytes[0], bytes[1]]);
        bytes = &bytes[UTF16_UNIT_BYTES..];
        let cp = if (HIGH_SURROGATE_START..=HIGH_SURROGATE_END).contains(&unit)
            && bytes.len() >= UTF16_UNIT_BYTES
        {
            let low = u16::from_le_bytes([bytes[0], bytes[1]]);
            if (LOW_SURROGATE_START..=LOW_SURROGATE_END).contains(&low) {
                bytes = &bytes[UTF16_UNIT_BYTES..];
                BMP_LIMIT
                    + (u32::from(unit - HIGH_SURROGATE_START) << SURROGATE_BITS)
                    + u32::from(low - LOW_SURROGATE_START)
            } else {
                u32::from(unit)
            }
        } else {
            u32::from(unit)
        };
        push_pair_cp(cp, name_out, lower_out);
    }
    true
}

/// True if folding would change `s` — i.e. the needle benefits from the case-insensitive pool at all.
#[must_use]
pub fn has_uppercase(s: &str) -> bool {
    s.chars().any(|c| fold_char(c) != c)
}

/// Position of an invalid or truncated WTF-8 sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// Byte offset of the rejected sequence.
    pub offset: usize,
}

const MIN_TWO_BYTE_LEAD: u8 = 0xC2;
const MAX_TWO_BYTE_LEAD: u8 = 0xDF;
const MAX_THREE_BYTE_LEAD: u8 = 0xEF;
const MAX_FOUR_BYTE_LEAD: u8 = 0xF4;
const TWO_BYTE_LEAD_MASK: u8 = 0x1F;
const THREE_BYTE_LEAD_MASK: u8 = 0x0F;
const FOUR_BYTE_LEAD_MASK: u8 = 0x07;

fn decode_point(bytes: &[u8], offset: usize) -> Result<(u32, usize), DecodeError> {
    let error = DecodeError { offset };
    let first = *bytes.get(offset).ok_or(error)?;
    let (width, mut point, minimum) = match first {
        0..=ASCII_LAST => return Ok((u32::from(first), 1)),
        MIN_TWO_BYTE_LEAD..=MAX_TWO_BYTE_LEAD => (
            TWO_BYTE_WIDTH,
            u32::from(first & TWO_BYTE_LEAD_MASK),
            ASCII_LIMIT,
        ),
        THREE_BYTE_TAG..=MAX_THREE_BYTE_LEAD => (
            THREE_BYTE_WIDTH,
            u32::from(first & THREE_BYTE_LEAD_MASK),
            TWO_BYTE_LIMIT,
        ),
        FOUR_BYTE_TAG..=MAX_FOUR_BYTE_LEAD => (
            FOUR_BYTE_WIDTH,
            u32::from(first & FOUR_BYTE_LEAD_MASK),
            BMP_LIMIT,
        ),
        _ => return Err(error),
    };
    let end = offset.checked_add(width).ok_or(error)?;
    let sequence = bytes.get(offset..end).ok_or(error)?;
    for &continuation in &sequence[1..] {
        if continuation & !CONTINUATION_MASK as u8 != CONTINUATION_TAG {
            return Err(error);
        }
        point = (point << CONTINUATION_BITS) | u32::from(continuation & CONTINUATION_MASK as u8);
    }
    if point < minimum || point > MAX_CODE_POINT {
        return Err(error);
    }
    Ok((point, width))
}

fn decode_units(bytes: &[u8], out: &mut [u16]) -> Result<usize, DecodeError> {
    if out.len() < bytes.len() {
        return Err(DecodeError { offset: 0 });
    }
    let mut offset = 0;
    while offset < bytes.len() {
        let (_, width) = decode_point(bytes, offset)?;
        offset += width;
    }
    offset = 0;
    let mut written = 0;
    while offset < bytes.len() {
        let (point, width) = decode_point(bytes, offset)?;
        offset += width;
        if point >= BMP_LIMIT {
            let value = point - BMP_LIMIT;
            out[written] = HIGH_SURROGATE_START + (value >> SURROGATE_BITS) as u16;
            out[written + 1] = LOW_SURROGATE_START + (value & SURROGATE_MASK) as u16;
            written += SURROGATE_PAIR_UNITS;
        } else {
            out[written] = point as u16;
            written += 1;
        }
    }
    Ok(written)
}

fn completed_output_len(initial: usize, written: Option<usize>) -> Option<usize> {
    initial.checked_add(written.unwrap_or(0))
}

/// Decode WTF-8 while preserving lone surrogates.
///
/// # Errors
///
/// Rejects truncated, overlong, invalid, or out-of-range encodings and restores the original output.
pub fn wtf8_to_utf16(bytes: &[u8], out: &mut Vec<u16>) -> Result<(), DecodeError> {
    let initial = out.len();
    let extended =
        completed_output_len(initial, Some(bytes.len())).ok_or(DecodeError { offset: 0 })?;
    out.resize(extended, 0);
    let result = decode_units(bytes, &mut out[initial..]);
    let retained = completed_output_len(initial, result.ok());
    let Some(length) = retained else {
        out.truncate(initial);
        return Err(DecodeError { offset: 0 });
    };
    out.truncate(length);
    result.map(|_| ())
}

#[cfg(kani)]
#[path = "wtf8_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_decoding_preserves_rejected_output_and_unused_units() {
        let original = [42; 4];
        let mut output = original;
        assert!(decode_units(b"A\xE0", &mut output).is_err());
        assert_eq!(output, original);
        assert!(decode_units(b"abc", &mut output[..2]).is_err());
        assert_eq!(output, original);
        assert_eq!(decode_units("😀".as_bytes(), &mut output), Ok(2));
        assert_eq!(output, [0xD83D, 0xDE00, 42, 42]);
    }

    #[test]
    fn utf8_length_classifier_pins_every_width_boundary() {
        assert_eq!(utf8_len(0x7F), 1);
        assert_eq!(utf8_len(0x80), 2);
        assert_eq!(utf8_len(0x7FF), 2);
        assert_eq!(utf8_len(0x800), 3);
        assert_eq!(utf8_len(0xFFFF), 3);
        assert_eq!(utf8_len(0x1_0000), 4);
    }

    #[test]
    fn malformed_sequences_preserve_the_output_prefix() {
        for bytes in [
            &b"A\xE0"[..],
            &b"\xC0\x80"[..],
            &b"\xF4\x90\x80\x80"[..],
            &b"\x80"[..],
        ] {
            let mut out = vec![42];
            assert!(wtf8_to_utf16(bytes, &mut out).is_err());
            assert_eq!(out, [42]);
        }
        let mut out = Vec::new();
        wtf8_to_utf16(&[0xED, 0xA0, 0x80], &mut out).expect("lone surrogate is WTF-8");
        assert_eq!(out, [0xD800]);
    }
}
