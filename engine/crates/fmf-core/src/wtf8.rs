//! WTF-8 name encoding and search-oriented case folding.
//!
//! NTFS file names are arbitrary u16 sequences and may contain unpaired
//! surrogates. Storing them as lossy UTF-8 would corrupt such names and make
//! them impossible to open from the UI. WTF-8 encodes
//! unpaired surrogates as their 3-byte sequences, is a superset of UTF-8, and
//! round-trips back to the original UTF-16.
//!
//! The index's folded dictionary and per-entry original overflow share a name
//! length (ADR-0003/0004), so the stored folded form must have exactly the same
//! byte length as the original. Storage folding therefore lowercases a code
//! point only when the result is one code point of identical encoded length.
//!
//! Canonical equivalence is deliberately a query-time view, not another
//! standing pool: ordinary non-ASCII literals compare NFC views while lone
//! surrogates remain opaque barriers. This keeps the ASCII pool sweep and
//! steady-state RAM unchanged while making NFC/NFD spellings equivalent.

use unicode_normalization::UnicodeNormalization;

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

/// Decode UTF-16 (with possible unpaired surrogates) into code points,
/// invoking `f(cp)` for each. ASCII units (`u < 0x80`, never a surrogate)
/// take a fast first arm that skips the pairing test.
#[inline]
fn for_each_code_point(units: &[u16], mut f: impl FnMut(u32)) {
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        let cp: u32 = if u < 0x80 {
            i += 1;
            u as u32
        } else if (0xD800..=0xDBFF).contains(&u)
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
        f(cp);
    }
}

/// Append the folded WTF-8 of one code point to `lower_out`: lowercase a real
/// scalar with the length-preserving rule (ADR-0004), mirror a lone surrogate
/// unchanged. The single fold-side definition shared by the two encoders.
#[inline]
fn push_folded_cp(cp: u32, lower_out: &mut Vec<u8>) {
    match char::from_u32(cp) {
        Some(c) => push_code_point(fold_char(c) as u32, lower_out),
        // Lone surrogate: no case to fold, mirror the original bytes.
        None => push_code_point(cp, lower_out),
    }
}

#[inline]
fn push_pair_cp(cp: u32, name_out: &mut Vec<u8>, lower_out: &mut Vec<u8>) {
    if cp < 0x80 {
        // ASCII dominates real names: one byte per pool, fold is plain
        // ASCII lowercase — skip scalar validation and width dispatch.
        let byte = cp as u8;
        name_out.push(byte);
        lower_out.push(byte.to_ascii_lowercase());
    } else {
        push_code_point(cp, name_out);
        push_folded_cp(cp, lower_out);
    }
}

/// Decode UTF-16 (with possible unpaired surrogates) and append both the
/// WTF-8 original and its folded form. The two outputs always grow by the
/// same number of bytes.
pub fn push_wtf8_pair(units: &[u16], name_out: &mut Vec<u8>, lower_out: &mut Vec<u8>) {
    for_each_code_point(units, |cp| push_pair_cp(cp, name_out, lower_out));
}

/// Encode a checked UTF-16LE byte slice without first materializing `u16`s.
///
/// The NTFS parser hands the initial-scan hot path a view into the record.
/// Rejecting odd byte counts before writing keeps malformed callers atomic;
/// valid NTFS `$FILE_NAME` payloads therefore incur no per-name scratch copy.
pub(crate) fn push_wtf8le_pair(
    bytes: &[u8],
    name_out: &mut Vec<u8>,
    lower_out: &mut Vec<u8>,
) -> bool {
    if !bytes.len().is_multiple_of(2) {
        return false;
    }
    let mut offset = 0usize;
    while offset < bytes.len() {
        let unit = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let cp = if (0xD800..=0xDBFF).contains(&unit) && offset + 3 < bytes.len() {
            let low = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
            if (0xDC00..=0xDFFF).contains(&low) {
                offset += 4;
                0x1_0000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00)
            } else {
                offset += 2;
                u32::from(unit)
            }
        } else {
            offset += 2;
            u32::from(unit)
        };
        push_pair_cp(cp, name_out, lower_out);
    }
    true
}

/// Fold a valid UTF-8 string (query needle) with the same rule as the pool.
pub fn fold_str(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

#[inline]
fn push_char(c: char, out: &mut Vec<u8>) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// Append one valid UTF-8 scalar span as NFC, optionally applying the index's
/// search fold after its first normalization pass. A second NFC pass is
/// required because Unicode casing can itself produce a decomposed sequence.
#[inline]
fn push_normalized_span(span: &str, folded: bool, out: &mut Vec<u8>) {
    if folded {
        for c in span.nfc().map(fold_char).nfc() {
            push_char(c, out);
        }
    } else {
        for c in span.nfc() {
            push_char(c, out);
        }
    }
}

#[inline]
fn surrogate_len(bytes: &[u8]) -> Option<usize> {
    (bytes.len() >= 3
        && bytes[0] == 0xED
        && (0xA0..=0xBF).contains(&bytes[1])
        && (0x80..=0xBF).contains(&bytes[2]))
    .then_some(3)
}

/// Materialize the canonical search view of a WTF-8 byte string.
///
/// Valid scalar spans are normalized independently to NFC. Every encoded lone
/// surrogate is copied byte-for-byte and terminates the normalization span on
/// both sides, so combining marks can never compose across an ill-formed
/// UTF-16 unit. Unexpected non-WTF-8 bytes are also copied as one-byte opaque
/// barriers; this makes the query boundary non-panicking even if an internally
/// corrupted snapshot reaches it.
///
/// ASCII is handled without invoking the Unicode tables. Callers reuse `out`,
/// so non-ASCII evaluation adds no standing index allocation.
pub(crate) fn normalize_wtf8_into(bytes: &[u8], folded: bool, out: &mut Vec<u8>) {
    out.clear();
    if bytes.is_ascii() {
        out.reserve(bytes.len());
        if folded {
            out.extend(bytes.iter().map(u8::to_ascii_lowercase));
        } else {
            out.extend_from_slice(bytes);
        }
        return;
    }

    out.reserve(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(span) => {
                push_normalized_span(span, folded, out);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid != 0 {
                    let span = std::str::from_utf8(&rest[..valid])
                        .expect("Utf8Error::valid_up_to guarantees a valid prefix");
                    push_normalized_span(span, folded, out);
                    rest = &rest[valid..];
                }

                if let Some(len) = surrogate_len(rest) {
                    out.extend_from_slice(&rest[..len]);
                    rest = &rest[len..];
                } else {
                    // A canonical WTF-8 name cannot reach this branch. Treat
                    // one invalid byte as an opaque boundary instead of
                    // panicking or dropping it.
                    out.push(rest[0]);
                    rest = &rest[1..];
                }
            }
        }
    }
}

/// Canonicalize one valid query string with the same view used for indexed
/// WTF-8 names.
pub(crate) fn normalize_str(s: &str, folded: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    push_normalized_span(s, folded, &mut out);
    out
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

    #[test]
    fn utf16le_encoder_rejects_odd_input_without_partial_output() {
        let mut name = vec![1];
        let mut lower = vec![2];
        assert!(!push_wtf8le_pair(&[b'A', 0, b'B'], &mut name, &mut lower));
        assert_eq!(name, [1]);
        assert_eq!(lower, [2]);
    }

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
    fn encoding_width_boundaries_are_exact() {
        // The first code point of each UTF-8 width must encode one byte wider
        // than the last of the previous width — pinning the `<` boundaries in
        // `push_code_point` (and, via round-trip, the lead-byte boundaries in
        // `wtf8_to_utf16`) against off-by-one drift.
        let cases: &[(u32, &[u8])] = &[
            (0x0000_007F, &[0x7F]),                   // last 1-byte
            (0x0000_0080, &[0xC2, 0x80]),             // first 2-byte
            (0x0000_07FF, &[0xDF, 0xBF]),             // last 2-byte
            (0x0000_0800, &[0xE0, 0xA0, 0x80]),       // first 3-byte
            (0x0000_FFFF, &[0xEF, 0xBF, 0xBF]),       // last 3-byte (BMP)
            (0x0001_0000, &[0xF0, 0x90, 0x80, 0x80]), // first 4-byte (astral)
            (0x0010_FFFF, &[0xF4, 0x8F, 0xBF, 0xBF]), // last code point
        ];
        for &(cp, want) in cases {
            let units: Vec<u16> = char::from_u32(cp)
                .unwrap()
                .to_string()
                .encode_utf16()
                .collect();
            let (name, lower) = pair(&units);
            assert_eq!(name, want, "U+{cp:04X} encodes to the wrong byte width");
            // None of these are cased, so the folded pool mirrors the original.
            assert_eq!(lower, want, "U+{cp:04X} fold must mirror an uncased point");
            let mut back = Vec::new();
            wtf8_to_utf16(&name, &mut back);
            assert_eq!(back, units, "U+{cp:04X} must round-trip back to UTF-16");
        }
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

    #[test]
    fn canonical_view_equates_nfc_nfd_and_mixed_forms() {
        let nfc = "Café/Ångström";
        let nfd = "Cafe\u{301}/A\u{30a}ngstro\u{308}m";
        assert_eq!(
            normalize_str(nfc, false),
            normalize_str(nfd, false),
            "canonical spelling must not affect exact literal matching"
        );
        assert_eq!(
            normalize_str("CAFÉ", true),
            normalize_str("CAFE\u{301}", true),
            "canonical spelling must not affect folded literal matching"
        );
    }

    #[test]
    fn lone_surrogate_is_an_opaque_normalization_barrier() {
        // "e<lone high surrogate>◌́": the accent must not jump across the
        // ill-formed UTF-16 unit and compose with the preceding e.
        let units = [b'e' as u16, 0xD800, 0x0301];
        let (name, _) = pair(&units);
        let mut canonical = Vec::new();
        normalize_wtf8_into(&name, false, &mut canonical);
        assert_eq!(canonical, [b'e', 0xED, 0xA0, 0x80, 0xCC, 0x81]);

        let mut back = Vec::new();
        wtf8_to_utf16(&name, &mut back);
        assert_eq!(
            back, units,
            "query-time normalization must never mutate stored WTF-8"
        );
    }

    #[test]
    fn canonical_view_is_idempotent_and_ascii_fast_path_is_exact() {
        for (text, folded) in [
            ("plain ASCII.TXT", false),
            ("plain ASCII.TXT", true),
            ("Café", false),
            ("Cafe\u{301}", false),
            ("ÉCOLE", true),
        ] {
            let once = normalize_str(text, folded);
            let mut twice = Vec::new();
            normalize_wtf8_into(&once, folded, &mut twice);
            assert_eq!(twice, once, "{text:?} canonical view is not stable");
        }
        assert_eq!(normalize_str("File.TXT", false), b"File.TXT");
        assert_eq!(normalize_str("File.TXT", true), b"file.txt");
    }
}

#[cfg(test)]
mod proptests {
    use proptest::collection::vec as prop_vec;
    use proptest::prelude::{Strategy, any};
    use proptest::sample::select;
    use proptest::{prop_assert, prop_assert_eq, prop_oneof, proptest};

    use super::{
        fold_str, has_uppercase, normalize_wtf8_into, push_wtf8_pair, push_wtf8le_pair,
        wtf8_to_utf16,
    };

    /// Code points whose folding stresses the length-preserving rule
    /// (ADR-0003): Turkish dotted I (İ lowercases to two chars → must be kept),
    /// dotless ı / ASCII I, German sharp-s (ß has no single-char lowering),
    /// full-width Latin (Ａ→ａ, both 3 bytes), a non-ASCII same-length foldable
    /// (Σ→σ), and Ⱥ (lowercases to a shorter encoding → must be kept). A `.*`
    /// strategy reaches these only by luck; pinning them keeps the hard cases
    /// in the input space on every run.
    const TRICKY_CHARS: &[char] = &[
        'İ', 'ı', 'I', 'i', 'ß', 'Ａ', 'ａ', 'Ｚ', 'Σ', 'σ', 'Ⱥ', 'A', 'z', '.',
    ];

    /// UTF-16 units that exercise surrogate handling: lone high/low surrogates
    /// (legal NTFS names, ill-formed UTF-16) plus the halves of an emoji
    /// surrogate pair, so the paired and unpaired branches both get hit.
    const TRICKY_UNITS: &[u16] = &[0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0xD83E, 0xDD80];

    /// A single UTF-16 unit drawn from the tricky set, a tricky char's units,
    /// or the whole `u16` space.
    fn tricky_unit() -> impl Strategy<Value = u16> {
        let char_units: Vec<u16> = TRICKY_CHARS
            .iter()
            .flat_map(|c| {
                let mut buf = [0u16; 2];
                c.encode_utf16(&mut buf).to_vec()
            })
            .collect();
        prop_oneof![
            select(TRICKY_UNITS.to_vec()),
            select(char_units),
            any::<u16>(),
        ]
    }

    /// A `Vec<u16>` heavily seeded with surrogates and tricky-fold chars.
    fn tricky_units() -> impl Strategy<Value = Vec<u16>> {
        prop_vec(tricky_unit(), 0usize..32)
    }

    /// A `String` built from the tricky-fold chars interleaved with arbitrary
    /// `char`s — always valid UTF-8, but rich in the fold edge cases.
    fn tricky_string() -> impl Strategy<Value = String> {
        prop_vec(
            prop_oneof![select(TRICKY_CHARS.to_vec()), any::<char>()],
            0usize..32,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        // The two pools grow by the same byte count for ANY UTF-16 name — the
        // shared-offset invariant (ADR-0003), across the whole input space.
        #[test]
        fn pools_same_length_for_any_units(units in prop_vec(any::<u16>(), 0usize..64)) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            prop_assert_eq!(name.len(), lower.len());
        }

        #[test]
        fn direct_utf16le_encoder_matches_u16_encoder(
            units in prop_vec(any::<u16>(), 0usize..64)
        ) {
            let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
            let (mut expected_name, mut expected_lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut expected_name, &mut expected_lower);
            let (mut actual_name, mut actual_lower) = (Vec::new(), Vec::new());
            prop_assert!(push_wtf8le_pair(
                &bytes,
                &mut actual_name,
                &mut actual_lower,
            ));
            prop_assert_eq!(actual_name, expected_name);
            prop_assert_eq!(actual_lower, expected_lower);
        }

        // Same shared-offset invariant, now forced onto the hard fold cases
        // (Turkish İ, ß, full-width Latin, surrogate pairs and lone surrogates)
        // every run instead of relying on `any::<u16>()` to stumble onto them.
        #[test]
        fn pools_same_length_for_tricky_units(units in tricky_units()) {
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

        // Round-trip on the surrogate-heavy generator: lone surrogates and
        // emoji pairs must survive the WTF-8 encode/decode unchanged.
        #[test]
        fn tricky_name_roundtrips_through_utf16(units in tricky_units()) {
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

        // Same length + idempotence, forced onto the tricky-fold chars — these
        // are exactly the code points where a non-length-preserving lowering
        // (İ→i̇, ß→ss, Ⱥ→its shorter form) would break the shared-offset pool.
        #[test]
        fn fold_str_length_preserving_and_idempotent_tricky(s in tricky_string()) {
            let folded = fold_str(&s);
            prop_assert_eq!(folded.len(), s.len());
            let twice = fold_str(&folded);
            prop_assert_eq!(twice, folded);
        }

        // `has_uppercase` is exactly the "folding changes something" predicate:
        // it must agree with `fold_str(s) != s` for ANY UTF-8 string. A
        // disagreement would desync smart-case from the pool it selects.
        #[test]
        fn has_uppercase_agrees_with_fold_changing(s in ".*") {
            prop_assert_eq!(has_uppercase(&s), fold_str(&s) != s);
        }

        // The same agreement on the fold edge cases, where a smart-case
        // mistake is most likely (İ is "uppercase" to Unicode but unfoldable by
        // our rule, so it must read as has_uppercase == false).
        #[test]
        fn has_uppercase_agrees_with_fold_changing_tricky(s in tricky_string()) {
            prop_assert_eq!(has_uppercase(&s), fold_str(&s) != s);
        }

        // The needle fold (`fold_str`) and the pool fold (`push_wtf8_pair`'s
        // lower output) must produce identical bytes for any well-formed UTF-8
        // name — otherwise a case-insensitive needle would not align with the
        // folded pool it scans. Restricted to inputs free of lone surrogates,
        // where `String` and the UTF-16 units agree.
        #[test]
        fn needle_fold_matches_pool_fold(s in ".*") {
            let units: Vec<u16> = s.encode_utf16().collect();
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            prop_assert_eq!(name, s.as_bytes());
            let folded = fold_str(&s);
            prop_assert_eq!(folded.as_bytes(), &lower[..]);
        }

        // A folded string never contains a code point our rule would still
        // fold — fixed-point reached in one pass (a sharper idempotence: not
        // just `fold(fold(s)) == fold(s)` but "nothing left to fold").
        #[test]
        fn folded_string_has_no_uppercase(s in tricky_string()) {
            prop_assert!(!has_uppercase(&fold_str(&s)));
        }

        // Canonical search views are stable for every UTF-16 sequence,
        // including arbitrary lone surrogates. The second pass must neither
        // panic nor move/drop an opaque barrier.
        #[test]
        fn canonical_wtf8_view_is_idempotent_for_any_units(
            units in prop_vec(any::<u16>(), 0usize..64),
            folded in any::<bool>(),
        ) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            let mut once = Vec::new();
            let mut twice = Vec::new();
            normalize_wtf8_into(&name, folded, &mut once);
            normalize_wtf8_into(&once, folded, &mut twice);
            prop_assert_eq!(twice, once);
        }

        // Surrogate code units remain byte-for-byte barriers in the canonical
        // view. Compare their ordered WTF-8 triples before and after rather
        // than decoding the normalized scalar spans, which may legitimately
        // compose and change their UTF-16 unit count.
        #[test]
        fn canonical_view_preserves_every_lone_surrogate_barrier(units in tricky_units()) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            let mut canonical = Vec::new();
            normalize_wtf8_into(&name, false, &mut canonical);
            let barriers = |bytes: &[u8]| {
                bytes.windows(3)
                    .filter(|w| {
                        w[0] == 0xED
                            && (0xA0..=0xBF).contains(&w[1])
                            && (0x80..=0xBF).contains(&w[2])
                    })
                    .map(<[u8; 3]>::try_from)
                    .collect::<Result<Vec<_>, _>>()
                    .expect("every window is exactly three bytes")
            };
            prop_assert_eq!(barriers(&canonical), barriers(&name));
        }

    }
}
