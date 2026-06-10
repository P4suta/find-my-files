use super::{RawEntry, VolumeIndex, VolumeIndexBuilder};

pub(super) fn raw<'a>(
    record: u64,
    parent: u64,
    name: &'a [u16],
    is_dir: bool,
    size: u64,
    mtime: i64,
) -> RawEntry<'a> {
    RawEntry {
        record,
        parent_record: parent,
        frn: (1u64 << 48) | record,
        name_utf16: name,
        is_dir,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size,
        mtime,
    }
}

pub(super) fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// C:\ ├─ docs\ ├─ note.txt   docs comes *after* its child in scan order.
pub(super) fn build_sample() -> VolumeIndex {
    let mut b = VolumeIndexBuilder::new("C:", 5);
    let note = u16s("Note.TXT");
    let docs = u16s("docs");
    let big = u16s("big.bin");
    b.push(raw(100, 50, &note, false, 10, 300)); // parent not yet pushed
    b.push(raw(50, 5, &docs, true, 0, 100));
    b.push(raw(60, 5, &big, false, 99_999, 200));
    b.finish()
}

pub(super) fn raw_attr<'a>(
    record: u64,
    parent: u64,
    name: &'a [u16],
    is_dir: bool,
    is_hidden: bool,
    is_system: bool,
) -> RawEntry<'a> {
    RawEntry {
        record,
        parent_record: parent,
        frn: (1u64 << 48) | record,
        name_utf16: name,
        is_dir,
        is_reparse: false,
        is_hidden,
        is_system,
        size: 0,
        mtime: 0,
    }
}
