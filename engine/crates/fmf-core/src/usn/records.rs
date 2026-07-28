//! Pure change-journal buffer parsing — no OS calls, so the whole layer is
//! testable from raw byte fixtures.
//!
//! That split is deliberate: reading a real journal needs elevation, so
//! keeping the grammar OS-free is what lets the USN logic be covered by
//! unelevated `cargo test` (see AGENTS.md).
//!
//! NTFS V2 records are decoded; foreign V3/V4 records are skipped only
//! after their version-specific variable-length layout has been validated.
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
    /// Reparse-point metadata changed (`USN_REASON_REPARSE_POINT_CHANGE`).
    pub const REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
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

fn valid_utf16_tail(
    record: &[u8],
    record_length: usize,
    fixed_header: usize,
    length_offset: usize,
    data_offset: usize,
) -> bool {
    let name_len = u16_at(record, length_offset) as usize;
    let name_off = u16_at(record, data_offset) as usize;
    name_off >= fixed_header
        && name_off.is_multiple_of(2)
        && name_len.is_multiple_of(2)
        && name_len <= 255 * 2
        && name_off
            .checked_add(name_len)
            .is_some_and(|end| end <= record_length)
}

fn valid_v4_extents(record: &[u8], record_length: usize) -> bool {
    const V4_HEADER_BYTES: usize = 64;
    const EXTENT_BYTES: usize = 16;

    if record_length < V4_HEADER_BYTES {
        return false;
    }
    let count = u16_at(record, 60) as usize;
    let extent_size = u16_at(record, 62) as usize;
    if count > 0 && extent_size < EXTENT_BYTES {
        return false;
    }
    count
        .checked_mul(extent_size)
        .and_then(|bytes| V4_HEADER_BYTES.checked_add(bytes))
        .is_some_and(|end| end <= record_length)
}

/// Parse a raw FSCTL output buffer.
///
/// Returns the leading "next" cursor value, the decoded records, and whether
/// trailing bytes had to be dropped (malformed/truncated input — callers
/// surface this as a counter+warning instead of letting it vanish).
#[must_use]
pub fn parse_buffer(buf: &[u8]) -> (u64, Vec<UsnRecord>, bool) {
    // A 64 KiB FSCTL buffer holds hundreds of ~60-80 B V2 records; pre-size
    // off the input length (min record ~60 B, ~96 B average) to skip the
    // realloc chain. Bounded by the input, so no over-allocation on a buffer
    // that decodes to few records.
    let mut records = Vec::with_capacity(buf.len() / 96);
    let mut truncated = false;
    if buf.len() < 8 {
        return (0, records, true);
    }
    let next = u64_at(buf, 0);
    let mut off = 8usize;

    while off + 60 <= buf.len() {
        let rec = &buf[off..];
        let record_length = u32_at(rec, 0) as usize;
        let Some(record_end) = off.checked_add(record_length) else {
            truncated = true;
            break;
        };
        if record_length < 60 || !record_length.is_multiple_of(8) || record_end > buf.len() {
            truncated = true;
            break;
        }
        match u16_at(rec, 4) {
            2 if valid_utf16_tail(rec, record_length, 60, 56, 58) => {
                let name_len = u16_at(rec, 56) as usize;
                let name_off = u16_at(rec, 58) as usize;
                let name_end = name_off + name_len;
                let mut name = Vec::with_capacity(name_len / 2);
                let nb = &rec[name_off..name_end];
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
            }
            2 => {
                // Name escapes its record: corrupt bytes. The record is
                // dropped, but the caller must hear about it (counter +
                // warning) — a silently lost rename means a stale index.
                truncated = true;
            }
            // A read can legally mix record versions. This NTFS indexer only
            // consumes V2's 64-bit file IDs; V3/V4 framing is nevertheless
            // validated before it is skipped. Treating a structurally valid
            // supported foreign version as corruption would create an
            // infinite full-rescan loop.
            3 if record_length >= 76 && valid_utf16_tail(rec, record_length, 76, 72, 74) => {}
            4 if valid_v4_extents(rec, record_length) => {}
            // Unknown versions and malformed foreign records cannot be
            // skipped safely: advancing the cursor could permanently lose a
            // record hidden behind corrupt framing.
            _ => truncated = true,
        }
        // FSCTL records are 8-byte aligned and RecordLength includes padding.
        off = record_end;
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
    fn malformed_record_fields_fail_closed() {
        let original = encode_buffer(9, &[rec(7, 5, reason::FILE_CREATE, "abc.txt")]);

        let mut odd_name_length = original.clone();
        odd_name_length[8 + 56..8 + 58].copy_from_slice(&3u16.to_le_bytes());
        let (_, parsed, malformed) = parse_buffer(&odd_name_length);
        assert!(malformed);
        assert!(parsed.is_empty());

        let mut header_name_offset = original.clone();
        header_name_offset[8 + 58..8 + 60].copy_from_slice(&58u16.to_le_bytes());
        let (_, parsed, malformed) = parse_buffer(&header_name_offset);
        assert!(malformed);
        assert!(parsed.is_empty());

        let mut unknown_version = original;
        unknown_version[8 + 4..8 + 6].copy_from_slice(&99u16.to_le_bytes());
        let (_, parsed, malformed) = parse_buffer(&unknown_version);
        assert!(malformed);
        assert!(parsed.is_empty());
    }

    #[test]
    fn structurally_valid_v3_and_v4_records_are_skipped() {
        let mut v3 = vec![0u8; 8 + 80];
        v3[..8].copy_from_slice(&9u64.to_le_bytes());
        v3[8..12].copy_from_slice(&80u32.to_le_bytes());
        v3[12..14].copy_from_slice(&3u16.to_le_bytes());
        v3[8 + 72..8 + 74].copy_from_slice(&2u16.to_le_bytes());
        v3[8 + 74..8 + 76].copy_from_slice(&76u16.to_le_bytes());
        v3[8 + 76..8 + 78].copy_from_slice(&u16::from(b'x').to_le_bytes());
        let (_, parsed, malformed) = parse_buffer(&v3);
        assert!(!malformed);
        assert!(parsed.is_empty());

        let mut v4 = vec![0u8; 8 + 80];
        v4[..8].copy_from_slice(&10u64.to_le_bytes());
        v4[8..12].copy_from_slice(&80u32.to_le_bytes());
        v4[12..14].copy_from_slice(&4u16.to_le_bytes());
        v4[8 + 60..8 + 62].copy_from_slice(&1u16.to_le_bytes());
        v4[8 + 62..8 + 64].copy_from_slice(&16u16.to_le_bytes());
        let (_, parsed, malformed) = parse_buffer(&v4);
        assert!(!malformed);
        assert!(parsed.is_empty());
    }

    #[test]
    fn malformed_v3_and_v4_records_fail_closed() {
        let mut short_v3 = vec![0u8; 8 + 72];
        short_v3[..8].copy_from_slice(&9u64.to_le_bytes());
        short_v3[8..12].copy_from_slice(&72u32.to_le_bytes());
        short_v3[12..14].copy_from_slice(&3u16.to_le_bytes());
        let (_, _, malformed) = parse_buffer(&short_v3);
        assert!(malformed);

        let mut truncated_extents = vec![0u8; 8 + 64];
        truncated_extents[..8].copy_from_slice(&10u64.to_le_bytes());
        truncated_extents[8..12].copy_from_slice(&64u32.to_le_bytes());
        truncated_extents[12..14].copy_from_slice(&4u16.to_le_bytes());
        truncated_extents[8 + 60..8 + 62].copy_from_slice(&1u16.to_le_bytes());
        truncated_extents[8 + 62..8 + 64].copy_from_slice(&16u16.to_le_bytes());
        let (_, _, malformed) = parse_buffer(&truncated_extents);
        assert!(malformed);
    }

    #[test]
    fn empty_buffer() {
        let (_, records, malformed) = parse_buffer(&[]);
        assert!(records.is_empty());
        assert!(malformed);

        let (next, records, malformed) = parse_buffer(&0u64.to_le_bytes());
        assert_eq!(next, 0);
        assert!(records.is_empty());
        assert!(!malformed);
    }
}
