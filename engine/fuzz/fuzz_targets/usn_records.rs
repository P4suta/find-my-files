#![no_main]
//! Fuzz the USN change-journal record parser over arbitrary bytes. On Windows
//! `parse_buffer` runs on the FSCTL output buffer; a record's `RecordLength` and
//! name offset/length are attacker-influenceable (a corrupt journal), so the
//! walk must terminate (every step advances) and never slice out of bounds.
//! Re-encode any parsed records to also exercise `encode_buffer`.

use fmf_core::usn::records::{encode_buffer, parse_buffer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (next, records, _truncated) = parse_buffer(data);
    if !records.is_empty() {
        let _ = encode_buffer(next, &records);
    }
});
