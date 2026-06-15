//! WTF-8 name encoding and search-oriented case folding.
//!
//! NTFS file names are arbitrary u16 sequences and may contain unpaired
//! surrogates. Storing them as lossy UTF-8 would corrupt such names and make
//! them impossible to open from the UI (docs/ARCHITECTURE.md). WTF-8 encodes
//! unpaired surrogates as their 3-byte sequences, is a superset of UTF-8, and
//! round-trips back to the original UTF-16.
//!
//! The index keeps two pools with shared offsets (docs/ARCHITECTURE.md,
//! ADR-0003), so the folded form of a name must have exactly the same byte
//! length as the original. Folding therefore lowercases a code point only
//! when the result is a single code point of identical encoded length;
//! everything else is kept as-is. The same rule must be applied to query
//! needles (`fold_str`) or case-insensitive matches would misalign.

/// Append the WTF-8 encoding of a single code point (may be a lone surrogate).
#[inline]
fn push_code_point(cp: u32, out: &mut Vec<u8>) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push(0xC0 | (cp >> 6) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else if cp < 0x1_0000 {
        out.push(0xE0 | (cp >> 12) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else {
        out.push(0xF0 | (cp >> 18) as u8);
        out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    }
}

#[inline]
const fn utf8_len(cp: u32) -> usize {
    match cp {
        0..0x80 => 1,
        0x80..0x800 => 2,
        0x800..0x1_0000 => 3,
        _ => 4,
    }
}

/// Lowercase `c` only if the result is a single char with the same encoded
/// length; otherwise return `c` unchanged.
#[inline]
fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) if utf8_len(l as u32) == utf8_len(c as u32) => l,
        _ => c,
    }
}

/// Decode UTF-16 (with possible unpaired surrogates) and append both the
/// WTF-8 original and its folded form. The two outputs always grow by the
/// same number of bytes.
pub fn push_wtf8_pair(units: &[u16], name_out: &mut Vec<u8>, lower_out: &mut Vec<u8>) {
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        let cp: u32 = if (0xD800..=0xDBFF).contains(&u)
            && i + 1 < units.len()
            && (0xDC00..=0xDFFF).contains(&units[i + 1])
        {
            let hi = (u as u32 - 0xD800) << 10;
            let lo = units[i + 1] as u32 - 0xDC00;
            i += 2;
            0x1_0000 + hi + lo
        } else {
            i += 1;
            u as u32
        };

        push_code_point(cp, name_out);
        match char::from_u32(cp) {
            Some(c) => push_code_point(fold_char(c) as u32, lower_out),
            // Lone surrogate: no case to fold, mirror the original bytes.
            None => push_code_point(cp, lower_out),
        }
    }
}

/// Fold a valid UTF-8 string (query needle) with the same rule as the pool.
pub fn fold_str(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

/// True if folding would change `s` — i.e. the needle benefits from the
/// case-insensitive pool at all.
#[must_use]
pub fn has_uppercase(s: &str) -> bool {
    s.chars().any(|c| fold_char(c) != c)
}

/// Decode WTF-8 back to UTF-16 units (inverse of `push_wtf8_pair`'s name
/// output). Used when handing names across the FFI boundary.
pub fn wtf8_to_utf16(bytes: &[u8], out: &mut Vec<u16>) {
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i] as u32;
        let (cp, adv) = if b0 < 0x80 {
            (b0, 1)
        } else if b0 < 0xE0 {
            ((b0 & 0x1F) << 6 | (bytes[i + 1] as u32 & 0x3F), 2)
        } else if b0 < 0xF0 {
            (
                (b0 & 0x0F) << 12
                    | (bytes[i + 1] as u32 & 0x3F) << 6
                    | (bytes[i + 2] as u32 & 0x3F),
                3,
            )
        } else {
            (
                (b0 & 0x07) << 18
                    | (bytes[i + 1] as u32 & 0x3F) << 12
                    | (bytes[i + 2] as u32 & 0x3F) << 6
                    | (bytes[i + 3] as u32 & 0x3F),
                4,
            )
        };
        i += adv;
        if cp >= 0x1_0000 {
            let v = cp - 0x1_0000;
            out.push(0xD800 + (v >> 10) as u16);
            out.push(0xDC00 + (v & 0x3FF) as u16);
        } else {
            out.push(cp as u16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(units: &[u16]) -> (Vec<u8>, Vec<u8>) {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        push_wtf8_pair(units, &mut a, &mut b);
        (a, b)
    }

    #[test]
    fn ascii_roundtrip_and_fold() {
        let units: Vec<u16> = "File.TXT".encode_utf16().collect();
        let (name, lower) = pair(&units);
        assert_eq!(name, b"File.TXT");
        assert_eq!(lower, b"file.txt");
    }

    #[test]
    fn pools_always_same_length() {
        for s in ["日本語ファイル.txt", "Straße", "İstanbul", "ΣΟΦΟΣ", "Ⱥb"] {
            let units: Vec<u16> = s.encode_utf16().collect();
            let (name, lower) = pair(&units);
            assert_eq!(name.len(), lower.len(), "length mismatch for {s}");
            assert_eq!(name, s.as_bytes());
        }
    }

    #[test]
    fn greek_sigma_folds_in_place() {
        // Σ (2 bytes) → σ (2 bytes): same length, folded.
        let units: Vec<u16> = "Σ".encode_utf16().collect();
        let (_, lower) = pair(&units);
        assert_eq!(lower, "σ".as_bytes());
    }

    #[test]
    fn istanbul_dotted_i_kept_unfolded() {
        // İ lowercases to "i\u{307}" (multi-char) → kept as-is.
        let units: Vec<u16> = "İ".encode_utf16().collect();
        let (name, lower) = pair(&units);
        assert_eq!(name, lower);
    }

    #[test]
    fn supplementary_plane_roundtrip() {
        let s = "𠮷野家🦀.txt"; // surrogate pairs in UTF-16
        let units: Vec<u16> = s.encode_utf16().collect();
        let (name, _) = pair(&units);
        assert_eq!(name, s.as_bytes());
        let mut back = Vec::new();
        wtf8_to_utf16(&name, &mut back);
        assert_eq!(back, units);
    }

    #[test]
    fn unpaired_surrogate_roundtrip() {
        // Legal as an NTFS name, impossible as UTF-8: must survive intact.
        let units = vec![0x0041, 0xD800, 0x0042]; // "A<lone high surrogate>B"
        let (name, lower) = pair(&units);
        assert_eq!(name.len(), lower.len());
        let mut back = Vec::new();
        wtf8_to_utf16(&name, &mut back);
        assert_eq!(back, units);
        // The ASCII letters around the surrogate still fold.
        assert_eq!(lower[0], b'a');
        assert_eq!(*lower.last().unwrap(), b'b');
    }

    #[test]
    fn fold_str_matches_pool_folding() {
        for s in ["File.TXT", "Straße", "İstanbul", "ΣΟΦΟΣ", "日本語"] {
            let units: Vec<u16> = s.encode_utf16().collect();
            let (_, lower) = pair(&units);
            assert_eq!(
                fold_str(s).as_bytes(),
                &lower[..],
                "needle/pool fold diverged for {s}"
            );
        }
    }

    #[test]
    fn has_uppercase_detects_foldable_chars_only() {
        assert!(has_uppercase("Abc"));
        assert!(has_uppercase("ΣΟΦΟΣ"));
        assert!(!has_uppercase("abc.txt"));
        assert!(!has_uppercase("日本語"));
        assert!(!has_uppercase("İ")); // unfoldable by our rule → not "uppercase" for smart case
    }
}

#[cfg(test)]
mod proptests {
    use proptest::collection::vec as prop_vec;
    use proptest::prelude::any;
    use proptest::{prop_assert_eq, proptest};

    use super::{fold_str, push_wtf8_pair, wtf8_to_utf16};

    proptest! {
        // The two pools grow by the same byte count for ANY UTF-16 name — the
        // shared-offset invariant (ADR-0003), across the whole input space.
        #[test]
        fn pools_same_length_for_any_units(units in prop_vec(any::<u16>(), 0usize..64)) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            prop_assert_eq!(name.len(), lower.len());
        }

        // The WTF-8 name output round-trips back to the original UTF-16 units
        // (including unpaired surrogates) for ANY input.
        #[test]
        fn name_roundtrips_through_utf16(units in prop_vec(any::<u16>(), 0usize..64)) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            let mut back = Vec::new();
            wtf8_to_utf16(&name, &mut back);
            prop_assert_eq!(back, units);
        }

        // `fold_str` preserves byte length and is idempotent for ANY UTF-8 string.
        #[test]
        fn fold_str_length_preserving_and_idempotent(s in ".*") {
            let folded = fold_str(&s);
            prop_assert_eq!(folded.len(), s.len());
            let twice = fold_str(&folded);
            prop_assert_eq!(twice, folded);
        }
    }
}
