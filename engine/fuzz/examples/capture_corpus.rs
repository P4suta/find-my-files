use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use fmf_runtime::index::{Frn, RawEntry, VolumeIndex, VolumeIndexBuilder};
use fmf_runtime::ondisk::ntfs::ROOT_RECORD;
use fmf_runtime::usn::records::{encode_buffer, parse_buffer, reason, UsnRecord};

fn preserve(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(bytes),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.file_type().is_file() && fs::read(path)? == bytes {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "preserve the existing regression and choose a new seed name",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

fn main() -> io::Result<()> {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let name: Vec<u16> = "é文件.bin".encode_utf16().collect();
    let mut builder = VolumeIndexBuilder::new_synthetic("C:", ROOT_RECORD);
    builder.push(RawEntry {
        parent_frn: Frn(ROOT_RECORD),
        frn: Frn(u64::MAX),
        name_utf16: &name,
        is_dir: false,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size: u64::MAX,
        mtime: 0,
    });
    let index = builder.finish();
    let mut snapshot = Vec::new();
    index.write_snapshot(&mut snapshot, u64::MAX, i64::MAX)?;
    let (decoded, journal, next) = VolumeIndex::read_snapshot(&mut snapshot.as_slice())?;
    assert_eq!(
        (decoded.len(), journal, next),
        (index.len(), u64::MAX, i64::MAX)
    );
    preserve(&corpus.join("index_snapshot/basic.bin"), &snapshot)?;

    let record = UsnRecord {
        usn: i64::MAX,
        frn: u64::MAX,
        parent_frn: ROOT_RECORD,
        reason: reason::FILE_CREATE | reason::CLOSE,
        attributes: 0,
        name,
    };
    let records = [record];
    let encoded = encode_buffer(u64::MAX, &records);
    let (next, decoded, truncated) = parse_buffer(&encoded);
    assert_eq!(next, u64::MAX);
    assert_eq!(decoded, records);
    assert!(!truncated);
    preserve(&corpus.join("usn_records/create.bin"), &encoded)?;
    Ok(())
}
