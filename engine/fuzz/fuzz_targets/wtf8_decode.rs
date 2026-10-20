#![no_main]

use fmf_core::wtf8::{push_wtf8_pair, wtf8_to_utf16};
use fmf_runtime::wtf8::{fold_str, has_uppercase};
use libfuzzer_sys::fuzz_target;

const OUTPUT_PREFIX: u16 = 0xFEFF;

fuzz_target!(|data: &[u8]| {
    let units: Vec<u16> = data
        .chunks_exact(size_of::<u16>())
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let mut name = Vec::new();
    let mut lower = Vec::new();
    push_wtf8_pair(&units, &mut name, &mut lower);
    assert_eq!(name.len(), lower.len());

    let mut decoded = Vec::new();
    wtf8_to_utf16(&name, &mut decoded).expect("encoded names decode");
    assert_eq!(decoded, units);

    let mut arbitrary = vec![OUTPUT_PREFIX];
    match wtf8_to_utf16(data, &mut arbitrary) {
        Ok(()) => {
            assert_eq!(arbitrary[0], OUTPUT_PREFIX);
            let mut encoded = Vec::new();
            let mut folded = Vec::new();
            push_wtf8_pair(&arbitrary[1..], &mut encoded, &mut folded);
            let mut roundtrip = Vec::new();
            wtf8_to_utf16(&encoded, &mut roundtrip).expect("re-encoded units decode");
            assert_eq!(roundtrip, arbitrary[1..]);
        }
        Err(error) => {
            assert_eq!(arbitrary, [OUTPUT_PREFIX]);
            assert!(error.offset < data.len());
        }
    }

    if let Ok(text) = std::str::from_utf8(data) {
        let folded = fold_str(text);
        assert_eq!(text.len(), folded.len());
        assert_eq!(fold_str(&folded), folded);
        assert_eq!(has_uppercase(text), folded != text);
    }
});
