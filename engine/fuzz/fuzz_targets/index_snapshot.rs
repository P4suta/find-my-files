#![no_main]
//! Fuzz the snapshot reader — the highest-risk decoder in fmf-core. It does
//! `unsafe` POD reads (`set_len` over `from_raw_parts_mut`) sized by untrusted
//! length prefixes in the on-disk `.fmfidx`. A corrupt or hostile snapshot must
//! come back as a clean `Err` (→ full-rescan fallback), never over-read,
//! over-allocate (the `try_reserve_exact` guard), or read uninitialized memory.

use fmf_core::index::VolumeIndex;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let mut cur = Cursor::new(data);
    let _ = VolumeIndex::read_snapshot(&mut cur);
});
