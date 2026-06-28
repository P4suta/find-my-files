#![no_main]
//! Fuzz the WTF-8 codec — the boundary that carries NTFS file names (potentially
//! ill-formed UTF-16 with unpaired surrogates) into the index. `wtf8_to_utf16`
//! over arbitrary bytes and the case-fold helpers must never panic or read out
//! of bounds on malformed encodings.

use fmf_core::wtf8::{fold_str, has_uppercase, push_wtf8_pair, wtf8_to_utf16};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Arbitrary bytes as a WTF-8 stream → UTF-16, then back through the encoder.
    let mut units = Vec::new();
    wtf8_to_utf16(data, &mut units);
    let mut name = Vec::new();
    let mut lower = Vec::new();
    push_wtf8_pair(&units, &mut name, &mut lower);

    // Case-fold helpers over any valid-UTF-8 prefix of the input.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = has_uppercase(s);
        let _ = fold_str(s);
    }
});
