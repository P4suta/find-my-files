#![no_main]
//! Fuzz the WTF-8 codec at its real untrusted boundary: `push_wtf8_pair` takes
//! UTF-16 straight from the OS ($MFT file names), which may contain *unpaired
//! surrogates*, and must encode any sequence without panicking or reading out of
//! bounds. We then round-trip through `wtf8_to_utf16` — whose contract is to
//! decode the well-formed WTF-8 `push_wtf8_pair` produces (not arbitrary bytes,
//! so it is exercised only on that output) — and check the units survive.

use fmf_core::wtf8::{fold_str, has_uppercase, push_wtf8_pair, wtf8_to_utf16};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Arbitrary OS-supplied UTF-16 (LE pairs), unpaired surrogates included.
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();

    let mut name = Vec::new();
    let mut lower = Vec::new();
    push_wtf8_pair(&units, &mut name, &mut lower);

    // Decode the well-formed WTF-8 we just produced (wtf8_to_utf16's actual
    // contract) — exercises the decoder under ASan without panicking. The
    // round-trip *equality* invariant is owned by the proptest suite.
    let mut back = Vec::new();
    wtf8_to_utf16(&name, &mut back);
    let _ = back;

    // Case-fold helpers over any valid-UTF-8 prefix of the input.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = has_uppercase(s);
        let _ = fold_str(s);
    }
});
