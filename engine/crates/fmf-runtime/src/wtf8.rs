//! Unicode normalization around the deterministic WTF-8 codec.

use fmf_core::wtf8::fold_char;
pub use fmf_core::wtf8::{has_uppercase, push_wtf8_pair, push_wtf8le_pair, wtf8_to_utf16};
use unicode_normalization::UnicodeNormalization;

/// Fold a valid UTF-8 string (query needle) with the same rule as the pool.
pub fn fold_str(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

#[inline]
fn push_char(c: char, out: &mut Vec<u8>) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// Append one valid UTF-8 scalar span as NFC, optionally applying the index's search fold after its first normalization pass.
/// A second NFC pass is required because Unicode casing can itself produce a decomposed sequence.
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

/// Materialize the canonical search view of a WTF-8 byte string.
///
/// Valid scalar spans are normalized independently to NFC.
/// Every encoded lone surrogate is copied byte-for-byte and terminates the normalization span on both sides, so combining marks can never compose across an ill-formed UTF-16 unit.
/// Unexpected non-WTF-8 bytes are also copied as one-byte opaque barriers; this makes the query boundary non-panicking even if an internally corrupted snapshot reaches it.
///
/// ASCII is handled without invoking the Unicode tables.
/// Callers reuse `out`,
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

                let (&byte, tail) = rest
                    .split_first()
                    .expect("the normalization loop has a non-empty remainder");
                out.push(byte);
                rest = tail;
            }
        }
    }
}

/// Canonicalize one valid query string with the same view used for indexed WTF-8 names.
pub(crate) fn normalize_str(s: &str, folded: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    push_normalized_span(s, folded, &mut out);
    out
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
        for s in [
            "日本語ファイル.txt",
            "Straße",
            "İstanbul",
            "\u{03a3}\u{039f}\u{03a6}\u{039f}\u{03a3}",
            "Ⱥb",
        ] {
            let units: Vec<u16> = s.encode_utf16().collect();
            let (name, lower) = pair(&units);
            assert_eq!(name.len(), lower.len(), "length mismatch for {s}");
            assert_eq!(name, s.as_bytes());
        }
    }

    #[test]
    fn greek_sigma_folds_in_place() {
        let units: Vec<u16> = "Σ".encode_utf16().collect();
        let (_, lower) = pair(&units);
        assert_eq!(lower, "σ".as_bytes());
    }

    #[test]
    fn encoding_width_boundaries_are_exact() {
        let cases: &[(u32, &[u8])] = &[
            (0x0000_007F, &[0x7F]),
            (0x0000_0080, &[0xC2, 0x80]),
            (0x0000_07FF, &[0xDF, 0xBF]),
            (0x0000_0800, &[0xE0, 0xA0, 0x80]),
            (0x0000_FFFF, &[0xEF, 0xBF, 0xBF]),
            (0x0001_0000, &[0xF0, 0x90, 0x80, 0x80]),
            (0x0010_FFFF, &[0xF4, 0x8F, 0xBF, 0xBF]),
        ];
        for &(cp, want) in cases {
            let units: Vec<u16> = char::from_u32(cp)
                .unwrap()
                .to_string()
                .encode_utf16()
                .collect();
            let (name, lower) = pair(&units);
            assert_eq!(name, want, "U+{cp:04X} encodes to the wrong byte width");
            assert_eq!(lower, want, "U+{cp:04X} fold must mirror an uncased point");
            let mut back = Vec::new();
            wtf8_to_utf16(&name, &mut back).expect("encoded name is valid WTF-8");
            assert_eq!(back, units, "U+{cp:04X} must round-trip back to UTF-16");
        }
    }

    #[test]
    fn istanbul_dotted_i_kept_unfolded() {
        let units: Vec<u16> = "İ".encode_utf16().collect();
        let (name, lower) = pair(&units);
        assert_eq!(name, lower);
    }

    #[test]
    fn supplementary_plane_roundtrip() {
        let s = "𠮷野家🦀.txt";
        let units: Vec<u16> = s.encode_utf16().collect();
        let (name, _) = pair(&units);
        assert_eq!(name, s.as_bytes());
        let mut back = Vec::new();
        wtf8_to_utf16(&name, &mut back).expect("encoded name is valid WTF-8");
        assert_eq!(back, units);
    }

    #[test]
    fn unpaired_surrogate_roundtrip() {
        let units = vec![0x0041, 0xD800, 0x0042];
        let (name, lower) = pair(&units);
        assert_eq!(name.len(), lower.len());
        let mut back = Vec::new();
        wtf8_to_utf16(&name, &mut back).expect("encoded name is valid WTF-8");
        assert_eq!(back, units);
        assert_eq!(lower[0], b'a');
        assert_eq!(*lower.last().unwrap(), b'b');
    }

    #[test]
    fn fold_str_matches_pool_folding() {
        for s in [
            "File.TXT",
            "Straße",
            "İstanbul",
            "\u{03a3}\u{039f}\u{03a6}\u{039f}\u{03a3}",
            "日本語",
        ] {
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
        assert!(has_uppercase("\u{03a3}\u{039f}\u{03a6}\u{039f}\u{03a3}"));
        assert!(!has_uppercase("abc.txt"));
        assert!(!has_uppercase("日本語"));
        assert!(!has_uppercase("İ"));
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
        let units = [b'e' as u16, 0xD800, 0x0301];
        let (name, _) = pair(&units);
        let mut canonical = Vec::new();
        normalize_wtf8_into(&name, false, &mut canonical);
        assert_eq!(canonical, [b'e', 0xED, 0xA0, 0x80, 0xCC, 0x81]);

        let mut back = Vec::new();
        wtf8_to_utf16(&name, &mut back).expect("encoded name is valid WTF-8");
        assert_eq!(
            back, units,
            "query-time normalization must never mutate stored WTF-8"
        );
    }

    #[test]
    fn canonical_view_normalizes_valid_prefix_before_lone_surrogate() {
        let units = [b'A' as u16, b'e' as u16, 0x0301, 0xD800, b'B' as u16];
        let (name, _) = pair(&units);
        let mut canonical = Vec::new();
        normalize_wtf8_into(&name, true, &mut canonical);
        assert_eq!(canonical, [b'a', 0xC3, 0xA9, 0xED, 0xA0, 0x80, b'b']);
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

    /// Code points whose folding stresses the length-preserving rule (ADR-0003): Turkish dotted I (İ lowercases to two chars → must be kept),
    /// dotless ı / ASCII I, German sharp-s (ß has no single-char lowering),
    /// full-width Latin (Ａ→ａ, both 3 bytes), a non-ASCII same-length foldable (Σ→σ), and Ⱥ (lowercases to a shorter encoding → must be kept).
    /// A `.*` strategy reaches these only by luck; pinning them keeps the hard cases in the input space on every run.
    const TRICKY_CHARS: &[char] = &[
        'İ', 'ı', 'I', 'i', 'ß', 'Ａ', 'ａ', 'Ｚ', 'Σ', 'σ', 'Ⱥ', 'A', 'z', '.',
    ];

    /// UTF-16 units that exercise surrogate handling: lone high/low surrogates (legal NTFS names, ill-formed UTF-16) plus the halves of an emoji surrogate pair, so the paired and unpaired branches both get hit.
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

    /// A `String` built from the tricky-fold chars interleaved with arbitrary `char`s — always valid UTF-8, but rich in the fold edge cases.
    fn tricky_string() -> impl Strategy<Value = String> {
        prop_vec(
            prop_oneof![select(TRICKY_CHARS.to_vec()), any::<char>()],
            0usize..32,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
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

        #[test]
        fn pools_same_length_for_tricky_units(units in tricky_units()) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            prop_assert_eq!(name.len(), lower.len());
        }

        #[test]
        fn name_roundtrips_through_utf16(units in prop_vec(any::<u16>(), 0usize..64)) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            let mut back = Vec::new();
            wtf8_to_utf16(&name, &mut back).expect("encoded name is valid WTF-8");
            prop_assert_eq!(back, units);
        }

        #[test]
        fn tricky_name_roundtrips_through_utf16(units in tricky_units()) {
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            let mut back = Vec::new();
            wtf8_to_utf16(&name, &mut back).expect("encoded name is valid WTF-8");
            prop_assert_eq!(back, units);
        }

        #[test]
        fn fold_str_length_preserving_and_idempotent(s in ".*") {
            let folded = fold_str(&s);
            prop_assert_eq!(folded.len(), s.len());
            let twice = fold_str(&folded);
            prop_assert_eq!(twice, folded);
        }

        #[test]
        fn fold_str_length_preserving_and_idempotent_tricky(s in tricky_string()) {
            let folded = fold_str(&s);
            prop_assert_eq!(folded.len(), s.len());
            let twice = fold_str(&folded);
            prop_assert_eq!(twice, folded);
        }

        #[test]
        fn has_uppercase_agrees_with_fold_changing(s in ".*") {
            prop_assert_eq!(has_uppercase(&s), fold_str(&s) != s);
        }

        #[test]
        fn has_uppercase_agrees_with_fold_changing_tricky(s in tricky_string()) {
            prop_assert_eq!(has_uppercase(&s), fold_str(&s) != s);
        }

        #[test]
        fn needle_fold_matches_pool_fold(s in ".*") {
            let units: Vec<u16> = s.encode_utf16().collect();
            let (mut name, mut lower) = (Vec::new(), Vec::new());
            push_wtf8_pair(&units, &mut name, &mut lower);
            prop_assert_eq!(name, s.as_bytes());
            let folded = fold_str(&s);
            prop_assert_eq!(folded.as_bytes(), &lower[..]);
        }

        #[test]
        fn folded_string_has_no_uppercase(s in tricky_string()) {
            prop_assert!(!has_uppercase(&fold_str(&s)));
        }

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
