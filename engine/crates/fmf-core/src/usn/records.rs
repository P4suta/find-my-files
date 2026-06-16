//! Pure `USN_RECORD_V2` buffer parsing — no OS calls, so the whole layer is
//! testable from raw byte fixtures (docs/ARCHITECTURE.md, CLAUDE.md elevation rules).
//!
//! Buffer layout returned by `FSCTL_READ_USN_JOURNAL` / `FSCTL_ENUM_USN_DATA`:
//! a leading u64 (the next USN / next FRN to resume from), then a sequence of
//! `USN_RECORD_V2` structures, each `RecordLength` bytes, 8-byte aligned.

/// Reason flags we act on (winioctl.h).
pub mod reason {
    /// File data was overwritten (`USN_REASON_DATA_OVERWRITE`).
    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    /// File data was extended (`USN_REASON_DATA_EXTEND`).
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    /// File data was truncated (`USN_REASON_DATA_TRUNCATION`).
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    /// Basic file info (attributes/timestamps) changed (`USN_REASON_BASIC_INFO_CHANGE`).
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    /// File or directory was created (`USN_REASON_FILE_CREATE`).
    pub const FILE_CREATE: u32 = 0x0000_0100;
    /// File or directory was deleted (`USN_REASON_FILE_DELETE`).
    pub const FILE_DELETE: u32 = 0x0000_0200;
    /// Record carries the name the file had before a rename (`USN_REASON_RENAME_OLD_NAME`).
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    /// Record carries the name the file has after a rename (`USN_REASON_RENAME_NEW_NAME`).
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    /// A hard link was added or removed (`USN_REASON_HARD_LINK_CHANGE`).
    pub const HARD_LINK_CHANGE: u32 = 0x0001_0000;
    /// Final record after a handle to the file was closed (`USN_REASON_CLOSE`).
    pub const CLOSE: u32 = 0x8000_0000;
}

/// Hidden-file attribute bit (`FILE_ATTRIBUTE_HIDDEN`).
pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
/// System-file attribute bit (`FILE_ATTRIBUTE_SYSTEM`).
pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
/// Directory attribute bit (`FILE_ATTRIBUTE_DIRECTORY`).
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// Reparse-point attribute bit (`FILE_ATTRIBUTE_REPARSE_POINT`), e.g. symlinks/junctions.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// One decoded journal record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecord {
    /// Update Sequence Number — this record's monotonic position in the journal.
    pub usn: i64,
    /// Full 64-bit FRN (with sequence).
    pub frn: u64,
    /// Full 64-bit FRN of the containing directory (with sequence).
    pub parent_frn: u64,
    /// Bitfield of `reason::*` flags describing what changed.
    pub reason: u32,
    /// Bitfield of `FILE_ATTRIBUTE_*` flags for the file at record time.
    pub attributes: u32,
    /// File name in UTF-16 units (single link name, see RESEARCH.md on
    /// hard links).
    pub name: Vec<u16>,
}

impl UsnRecord {
    /// True if this record is for a directory.
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    /// True if this record is for a reparse point (symlink/junction).
    #[must_use]
    pub const fn is_reparse(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    /// True if the hidden attribute is set.
    #[must_use]
    pub const fn is_hidden(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_HIDDEN != 0
    }
    /// True if the system attribute is set.
    #[must_use]
    pub const fn is_system(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_SYSTEM != 0
    }
}

#[inline]
fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
#[inline]
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
#[inline]
fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

/// Parse a raw FSCTL output buffer.
///
/// Returns the leading "next" cursor value, the decoded records, and whether
/// trailing bytes had to be dropped (malformed/truncated input — callers
/// surface this as a counter+warning instead of letting it vanish).
#[must_use]
pub fn parse_buffer(buf: &[u8]) -> (u64, Vec<UsnRecord>, bool) {
    let mut records = Vec::new();
    let mut truncated = false;
    if buf.len() < 8 {
        return (0, records, !buf.is_empty());
    }
    let next = u64_at(buf, 0);
    let mut off = 8usize;

    while off + 60 <= buf.len() {
        let rec = &buf[off..];
        let record_length = u32_at(rec, 0) as usize;
        if record_length < 60 || off + record_length > buf.len() {
            truncated = true;
            break;
        }
        let major = u16_at(rec, 4);
        if major == 2 {
            let name_len = u16_at(rec, 56) as usize; // bytes
            let name_off = u16_at(rec, 58) as usize;
            if name_off + name_len <= record_length {
                let mut name = Vec::with_capacity(name_len / 2);
                let nb = &rec[name_off..name_off + name_len];
                for ch in nb.chunks_exact(2) {
                    name.push(u16::from_le_bytes([ch[0], ch[1]]));
                }
                records.push(UsnRecord {
                    usn: u64_at(rec, 24) as i64,
                    frn: u64_at(rec, 8),
                    parent_frn: u64_at(rec, 16),
                    reason: u32_at(rec, 40),
                    attributes: u32_at(rec, 52),
                    name,
                });
            } else {
                // Name escapes its record: corrupt bytes. The record is
                // dropped, but the caller must hear about it (counter +
                // warning) — a silently lost rename means a stale index.
                truncated = true;
            }
        }
        // Records are 8-byte aligned; RecordLength already includes padding.
        off += record_length.next_multiple_of(8);
    }
    if off != buf.len() {
        truncated = true; // sub-record trailing garbage (< 60 bytes)
    }
    (next, records, truncated)
}

/// Serialize records into the FSCTL wire format — used to build test
/// fixtures and replay files (`fmf capture-usn`).
#[must_use]
pub fn encode_buffer(next: u64, records: &[UsnRecord]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&next.to_le_bytes());
    for r in records {
        let name_bytes: Vec<u8> = r.name.iter().flat_map(|u| u.to_le_bytes()).collect();
        let len = (60 + name_bytes.len()).next_multiple_of(8);
        let start = out.len();
        out.resize(start + len, 0);
        let w = &mut out[start..];
        w[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        w[4..6].copy_from_slice(&2u16.to_le_bytes()); // major
        w[8..16].copy_from_slice(&r.frn.to_le_bytes());
        w[16..24].copy_from_slice(&r.parent_frn.to_le_bytes());
        w[24..32].copy_from_slice(&(r.usn as u64).to_le_bytes());
        w[40..44].copy_from_slice(&r.reason.to_le_bytes());
        w[52..56].copy_from_slice(&r.attributes.to_le_bytes());
        w[56..58].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        w[58..60].copy_from_slice(&60u16.to_le_bytes());
        w[60..60 + name_bytes.len()].copy_from_slice(&name_bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frn: u64, parent: u64, reason: u32, name: &str) -> UsnRecord {
        UsnRecord {
            usn: 1000,
            frn,
            parent_frn: parent,
            reason,
            attributes: 0x20,
            name: name.encode_utf16().collect(),
        }
    }

    #[test]
    fn roundtrip() {
        let records = vec![
            rec(
                0x1_0000_0000_0007,
                5,
                reason::FILE_CREATE | reason::CLOSE,
                "new file.txt",
            ),
            rec(
                0x2_0000_0000_0008,
                5,
                reason::FILE_DELETE | reason::CLOSE,
                "夢.dat",
            ),
        ];
        let buf = encode_buffer(42, &records);
        let (next, parsed, truncated) = parse_buffer(&buf);
        assert!(!truncated);
        assert_eq!(next, 42);
        assert_eq!(parsed, records);
    }

    #[test]
    fn truncated_tail_is_dropped() {
        let records = vec![rec(7, 5, reason::FILE_CREATE, "abc.txt")];
        let mut buf = encode_buffer(9, &records);
        buf.truncate(buf.len() - 4);
        let (next, parsed, truncated) = parse_buffer(&buf);
        assert!(truncated);
        assert_eq!(next, 9);
        assert!(parsed.is_empty());
    }

    #[test]
    fn empty_buffer() {
        assert_eq!(parse_buffer(&[]).1, vec![]);
        assert_eq!(parse_buffer(&7u64.to_le_bytes()).1, vec![]);
    }
}
