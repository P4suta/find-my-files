#![no_main]
//! Check snapshot validation and stable re-encoding over untrusted bytes.

use fmf_runtime::index::VolumeIndex;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let mut cur = Cursor::new(data);
    if let Ok((index, journal, next_usn)) = VolumeIndex::read_snapshot(&mut cur) {
        let mut encoded = Vec::new();
        index
            .write_snapshot(&mut encoded, journal, next_usn)
            .expect("decoded snapshot encodes");
        let (again, again_journal, again_usn) =
            VolumeIndex::read_snapshot(&mut Cursor::new(&encoded))
                .expect("encoded snapshot decodes");
        assert_eq!(
            (again_journal, again_usn, again.len()),
            (journal, next_usn, index.len())
        );
        let mut canonical = Vec::new();
        again
            .write_snapshot(&mut canonical, again_journal, again_usn)
            .expect("snapshot re-encodes");
        assert_eq!(canonical, encoded);
    }
});
