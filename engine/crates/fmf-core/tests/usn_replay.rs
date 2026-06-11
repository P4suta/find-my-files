//! USN replay integration tests (CLAUDE.md: USNロジックはfixtureリプレイで
//! テストする). Synthetic USN_RECORD_V2 buffers are built byte-by-byte from
//! the documented winioctl.h layout (docs/RESEARCH.md →
//! https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2)
//! — independently of `records::encode_buffer` — then run through the full
//! non-OS pipeline: raw bytes → `parse_buffer` → `apply_batch` → index.
//!
//! The `StatFetcher` trait is the existing test seam standing in for the
//! OS-backed `VolumeStatFetcher` (size/mtime are absent from USN records).

use std::collections::HashMap;

use fmf_core::index::{RawEntry, VolumeIndex, VolumeIndexBuilder};
use fmf_core::usn::records::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, encode_buffer,
};
use fmf_core::usn::{StatFetcher, UsnRecord, apply_batch, parse_buffer, reason};

/// FILE_ATTRIBUTE_ARCHIVE — the "plain file" attribute value.
const ARCHIVE: u32 = 0x20;

// ── Synthetic USN_RECORD_V2 builder ─────────────────────────────────────
//
// USN_RECORD_V2 layout (winioctl.h, all little-endian):
//   offset  0  DWORD     RecordLength   (60 + FileNameLength, 8-byte aligned)
//   offset  4  WORD      MajorVersion   (2)
//   offset  6  WORD      MinorVersion   (0)
//   offset  8  DWORDLONG FileReferenceNumber
//   offset 16  DWORDLONG ParentFileReferenceNumber
//   offset 24  USN       Usn            (LONGLONG)
//   offset 32  LARGE_INTEGER TimeStamp  (journal time — not indexed)
//   offset 40  DWORD     Reason
//   offset 44  DWORD     SourceInfo
//   offset 48  DWORD     SecurityId
//   offset 52  DWORD     FileAttributes
//   offset 56  WORD      FileNameLength (bytes)
//   offset 58  WORD      FileNameOffset (60)
//   offset 60  WCHAR[]   FileName       (UTF-16LE, not NUL-terminated)

/// offsetof(USN_RECORD_V2, FileName).
const NAME_OFFSET: usize = 60;

/// Specification of one synthetic record; every field the parser consumes is
/// settable, the ignored ones (TimeStamp/SourceInfo/SecurityId) are filled
/// with non-zero noise to prove they really are ignored.
struct RecSpec<'a> {
    usn: i64,
    frn: u64,
    parent_frn: u64,
    reason: u32,
    attributes: u32,
    name: &'a str,
}

fn encode_record_v2(out: &mut Vec<u8>, spec: &RecSpec) {
    let name_units: Vec<u16> = spec.name.encode_utf16().collect();
    let name_bytes = name_units.len() * 2;
    // RecordLength covers header + name, rounded up to 8-byte alignment.
    let record_length = (NAME_OFFSET + name_bytes).next_multiple_of(8);
    let start = out.len();
    out.resize(start + record_length, 0);
    let w = &mut out[start..];
    w[0..4].copy_from_slice(&(record_length as u32).to_le_bytes());
    w[4..6].copy_from_slice(&2u16.to_le_bytes()); // MajorVersion
    w[6..8].copy_from_slice(&0u16.to_le_bytes()); // MinorVersion
    w[8..16].copy_from_slice(&spec.frn.to_le_bytes());
    w[16..24].copy_from_slice(&spec.parent_frn.to_le_bytes());
    w[24..32].copy_from_slice(&spec.usn.to_le_bytes());
    w[32..40].copy_from_slice(&0x01DC_BEEF_F00D_4242i64.to_le_bytes()); // TimeStamp (noise)
    w[40..44].copy_from_slice(&spec.reason.to_le_bytes());
    w[44..48].copy_from_slice(&0xAAAA_AAAAu32.to_le_bytes()); // SourceInfo (noise)
    w[48..52].copy_from_slice(&0x5555_5555u32.to_le_bytes()); // SecurityId (noise)
    w[52..56].copy_from_slice(&spec.attributes.to_le_bytes());
    w[56..58].copy_from_slice(&(name_bytes as u16).to_le_bytes());
    w[58..60].copy_from_slice(&(NAME_OFFSET as u16).to_le_bytes());
    for (i, unit) in name_units.iter().enumerate() {
        let off = NAME_OFFSET + i * 2;
        w[off..off + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// Full FSCTL_READ_USN_JOURNAL output buffer: leading u64 (next USN to
/// resume from) followed by 8-byte-aligned records.
fn usn_buffer(next_usn: u64, specs: &[RecSpec]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&next_usn.to_le_bytes());
    for s in specs {
        encode_record_v2(&mut out, s);
    }
    out
}

// ── Replay helpers ───────────────────────────────────────────────────────

/// Full FRN for a record number: sequence 1 in the top 16 bits.
fn frn(record: u64) -> u64 {
    (1 << 48) | record
}

/// Canned size/mtime answers keyed by full FRN — the replay stand-in for
/// `VolumeStatFetcher`.
struct MapFetcher(HashMap<u64, (u64, i64)>);

impl StatFetcher for MapFetcher {
    fn stat(&self, frn: u64) -> Option<(u64, i64)> {
        self.0.get(&frn).copied()
    }
}

fn no_stats() -> MapFetcher {
    MapFetcher(HashMap::new())
}

/// Parse a synthetic buffer (asserting it is well formed) and apply it.
fn replay(
    idx: &mut VolumeIndex,
    next_usn: u64,
    specs: &[RecSpec],
    fetch: &dyn StatFetcher,
) -> fmf_core::usn::BatchStats {
    let buf = usn_buffer(next_usn, specs);
    let (next, records, truncated) = parse_buffer(&buf);
    assert_eq!(next, next_usn, "leading cursor must round-trip");
    assert!(!truncated, "well-formed fixture flagged as truncated");
    assert_eq!(records.len(), specs.len(), "every record must parse");
    apply_batch(idx, &records, fetch)
}

/// C:\ ├─ docs\ (rec 10) │ └─ note.txt (rec 11, 100B) ├─ archive\ (rec 20).
fn base_index() -> VolumeIndex {
    let mut b = VolumeIndexBuilder::new("C:", 5);
    let mut push = |record: u64, parent: u64, name: &str, is_dir: bool, size: u64, mtime: i64| {
        let units: Vec<u16> = name.encode_utf16().collect();
        b.push(RawEntry {
            record,
            parent_record: parent,
            frn: frn(record),
            name_utf16: &units,
            is_dir,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size,
            mtime,
        });
    };
    push(10, 5, "docs", true, 0, 0);
    push(11, 10, "note.txt", false, 100, 7);
    push(20, 5, "archive", true, 0, 0);
    b.finish()
}

fn path_of(idx: &VolumeIndex, record: u64) -> String {
    let id = idx.entry_by_record(record).expect("record in index");
    let mut p = Vec::new();
    idx.append_path(id, &mut p);
    String::from_utf8(p).expect("WTF-8 paths in these fixtures are UTF-8")
}

fn live_names(idx: &VolumeIndex) -> Vec<String> {
    (0..idx.len() as u32)
        .filter(|&id| idx.is_live(id))
        .map(|id| String::from_utf8_lossy(idx.name(id)).into_owned())
        .collect()
}

// ── Builder fidelity ─────────────────────────────────────────────────────

#[test]
fn hand_built_layout_parses_field_for_field() {
    // Odd-length name → 60 + 14 = 74 → RecordLength pads to 80.
    let spec = RecSpec {
        usn: 0x0123_4567_89AB,
        frn: frn(42),
        parent_frn: frn(5),
        reason: reason::FILE_CREATE | reason::CLOSE,
        attributes: FILE_ATTRIBUTE_HIDDEN | ARCHIVE,
        name: "夢n.txt1", // 7 UTF-16 units, exercises non-ASCII + padding
    };
    let buf = usn_buffer(7777, &[spec]);
    assert_eq!(buf.len(), 8 + 80, "alignment padding must land at 80");

    let (next, records, truncated) = parse_buffer(&buf);
    assert!(!truncated);
    assert_eq!(next, 7777);
    let expect = UsnRecord {
        usn: 0x0123_4567_89AB,
        frn: frn(42),
        parent_frn: frn(5),
        reason: reason::FILE_CREATE | reason::CLOSE,
        attributes: FILE_ATTRIBUTE_HIDDEN | ARCHIVE,
        name: "夢n.txt1".encode_utf16().collect(),
    };
    assert_eq!(records, vec![expect.clone()]);
    assert!(records[0].is_hidden() && !records[0].is_dir());

    // Cross-check the independent builder against the crate's own encoder
    // (TimeStamp/SourceInfo/SecurityId are noise here, zero there — the
    // parser must treat both identically).
    let (n2, r2, t2) = parse_buffer(&encode_buffer(7777, &[expect]));
    assert_eq!((n2, &r2, t2), (next, &records, false));
}

// ── Scenario a: rename storm ─────────────────────────────────────────────

#[test]
fn rename_storm_replay_keeps_only_the_final_name() {
    let mut idx = base_index();
    let live_before = idx.live_len();
    let g0 = idx.content_generation();
    let storm = |usn: i64, r: u32, name: &'static str| RecSpec {
        usn,
        frn: frn(11),
        parent_frn: frn(10),
        reason: r,
        attributes: ARCHIVE,
        name,
    };
    let stats = replay(
        &mut idx,
        6000,
        &[
            storm(1000, reason::RENAME_OLD_NAME, "note.txt"),
            storm(2000, reason::RENAME_NEW_NAME, "step1.tmp"),
            storm(3000, reason::RENAME_OLD_NAME, "step1.tmp"),
            storm(4000, reason::RENAME_NEW_NAME, "step2.tmp"),
            storm(5000, reason::RENAME_OLD_NAME, "step2.tmp"),
            storm(6000, reason::RENAME_NEW_NAME | reason::CLOSE, "最終版.txt"),
        ],
        &no_stats(),
    );

    // The whole storm collapses to one upsert carrying the final name.
    assert_eq!(stats.created_or_renamed, 1);
    assert_eq!(idx.live_len(), live_before);
    assert_eq!(path_of(&idx, 11), "C:\\docs\\最終版.txt");
    let names = live_names(&idx);
    for stale in ["note.txt", "step1.tmp", "step2.tmp"] {
        assert!(!names.contains(&stale.to_string()), "{stale} still live");
    }
    // Size/mtime carry over from the replaced entry without a fetcher.
    let id = idx.entry_by_record(11).unwrap();
    assert_eq!((idx.size(id), idx.mtime(id)), (100, 7));
    // Exactly one content-generation bump per batch.
    assert_eq!(idx.content_generation(), g0 + 1);
}

// ── Scenario b: directory move ───────────────────────────────────────────

#[test]
fn directory_move_replay_updates_descendant_paths() {
    let mut idx = base_index();
    // docs\ (10) moves under archive\ (20): NTFS emits a rename pair for the
    // directory record only — no records for note.txt underneath.
    let mv = |usn: i64, r: u32, parent: u64| RecSpec {
        usn,
        frn: frn(10),
        parent_frn: frn(parent),
        reason: r,
        attributes: FILE_ATTRIBUTE_DIRECTORY,
        name: "docs",
    };
    let stats = replay(
        &mut idx,
        2000,
        &[
            mv(1000, reason::RENAME_OLD_NAME, 5),
            mv(2000, reason::RENAME_NEW_NAME | reason::CLOSE, 20),
        ],
        &no_stats(),
    );

    assert_eq!(stats.created_or_renamed, 1);
    // The directory keeps its EntryId (children point at it), so the lazy
    // path of the untouched child follows the move.
    assert_eq!(path_of(&idx, 10), "C:\\archive\\docs");
    assert_eq!(path_of(&idx, 11), "C:\\archive\\docs\\note.txt");
}

// ── Scenario c: create → delete ──────────────────────────────────────────

#[test]
fn create_then_delete_replay_removes_the_entry_again() {
    let mut idx = base_index();
    let live_before = idx.live_len();

    // Batch 1: creation; size/mtime come from the (injected) stat fetcher.
    let fetch = MapFetcher(HashMap::from([(frn(30), (123, 456))]));
    let stats = replay(
        &mut idx,
        1001,
        &[RecSpec {
            usn: 1000,
            frn: frn(30),
            parent_frn: frn(10),
            reason: reason::FILE_CREATE | reason::CLOSE,
            attributes: ARCHIVE,
            name: "ghost.tmp",
        }],
        &fetch,
    );
    assert_eq!(stats.created_or_renamed, 1);
    assert_eq!(stats.stat_failures, 0);
    assert_eq!(idx.live_len(), live_before + 1);
    let id = idx.entry_by_record(30).expect("created entry is live");
    assert_eq!((idx.size(id), idx.mtime(id)), (123, 456));
    assert_eq!(path_of(&idx, 30), "C:\\docs\\ghost.tmp");

    // Batch 2: deletion of the same FRN.
    let stats = replay(
        &mut idx,
        2001,
        &[RecSpec {
            usn: 2000,
            frn: frn(30),
            parent_frn: frn(10),
            reason: reason::FILE_DELETE | reason::CLOSE,
            attributes: ARCHIVE,
            name: "ghost.tmp",
        }],
        &no_stats(),
    );
    assert_eq!(stats.deleted, 1);
    assert_eq!(idx.entry_by_record(30), None);
    assert_eq!(idx.live_len(), live_before);
}

// ── Scenario d: attribute change → EXCLUDED bit ──────────────────────────

#[test]
fn basic_info_change_replay_toggles_excluded_bit() {
    let mut idx = base_index();
    let id = idx.entry_by_record(11).unwrap();
    assert!(!idx.is_excluded(id), "fixture starts plain");

    let flip = |usn: i64, attributes: u32| RecSpec {
        usn,
        frn: frn(11),
        parent_frn: frn(10),
        reason: reason::BASIC_INFO_CHANGE | reason::CLOSE,
        attributes,
        name: "note.txt",
    };

    // hidden+system on → excluded; the same batch also refreshes size/mtime.
    let fetch = MapFetcher(HashMap::from([(frn(11), (5000, 99))]));
    let stats = replay(
        &mut idx,
        1001,
        &[flip(
            1000,
            FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM | ARCHIVE,
        )],
        &fetch,
    );
    assert_eq!(stats.stat_updated, 1);
    let id = idx.entry_by_record(11).unwrap();
    assert!(idx.is_excluded(id), "hidden|system must set EXCLUDED");
    assert_eq!((idx.size(id), idx.mtime(id)), (5000, 99));

    // back to a plain archive file → bit clears even when the stat fetch
    // fails (attribute updates must not depend on the volume answering).
    let stats = replay(&mut idx, 2001, &[flip(2000, ARCHIVE)], &no_stats());
    assert_eq!(stats.stat_failures, 1);
    let id = idx.entry_by_record(11).unwrap();
    assert!(
        !idx.is_excluded(id),
        "EXCLUDED must clear with the attributes"
    );
}

// ── Scenario e: malformed tails ──────────────────────────────────────────

#[test]
fn truncated_tail_replay_keeps_whole_records_and_flags_the_loss() {
    let mut idx = base_index();
    let live_before = idx.live_len();
    let buf = usn_buffer(
        9000,
        &[
            RecSpec {
                usn: 1000,
                frn: frn(40),
                parent_frn: frn(10),
                reason: reason::FILE_CREATE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "kept.txt",
            },
            RecSpec {
                usn: 2000,
                frn: frn(41),
                parent_frn: frn(10),
                reason: reason::FILE_CREATE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "casualty.txt",
            },
        ],
    );

    // Cut into the middle of the second record (still ≥60 bytes of it left,
    // so the parser sees its header but not its full RecordLength).
    let cut = &buf[..buf.len() - 4];
    let (next, records, truncated) = parse_buffer(cut);
    assert!(truncated, "lost tail bytes must be flagged");
    assert_eq!(next, 9000);
    assert_eq!(records.len(), 1, "only the complete record survives");
    assert_eq!(String::from_utf16(&records[0].name).unwrap(), "kept.txt");

    // The surviving prefix still applies cleanly.
    let stats = apply_batch(&mut idx, &records, &no_stats());
    assert_eq!(stats.created_or_renamed, 1);
    assert_eq!(idx.live_len(), live_before + 1);
    assert!(idx.entry_by_record(40).is_some());
    assert_eq!(idx.entry_by_record(41), None);
}

#[test]
fn corrupt_record_length_replay_stops_without_panic() {
    // A valid record followed by a record whose RecordLength (16) is smaller
    // than the fixed header — the parser must flag and stop, not loop or
    // panic.
    let mut buf = usn_buffer(
        500,
        &[RecSpec {
            usn: 100,
            frn: frn(50),
            parent_frn: frn(5),
            reason: reason::FILE_CREATE | reason::CLOSE,
            attributes: ARCHIVE,
            name: "ok.txt",
        }],
    );
    let mut junk = vec![0u8; 64];
    junk[0..4].copy_from_slice(&16u32.to_le_bytes());
    junk[4..6].copy_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&junk);

    let (next, records, truncated) = parse_buffer(&buf);
    assert!(truncated);
    assert_eq!(next, 500);
    assert_eq!(records.len(), 1);
    assert_eq!(String::from_utf16(&records[0].name).unwrap(), "ok.txt");
}

#[test]
fn name_escaping_its_record_is_dropped_flagged_and_following_records_survive() {
    // FileNameLength pointing past RecordLength: the record is skipped (the
    // parser does not read out of bounds), parsing resumes at the next
    // record, and the malformed-bytes flag is raised so the loss reaches
    // the usn_batches_truncated counter instead of vanishing.
    let mut buf = usn_buffer(
        700,
        &[
            RecSpec {
                usn: 100,
                frn: frn(60),
                parent_frn: frn(5),
                reason: reason::FILE_CREATE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "bad.txt",
            },
            RecSpec {
                usn: 200,
                frn: frn(61),
                parent_frn: frn(5),
                reason: reason::FILE_CREATE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "good.txt",
            },
        ],
    );
    // First record starts right after the 8-byte lead; corrupt its
    // FileNameLength field (offset 56 inside the record).
    buf[8 + 56..8 + 58].copy_from_slice(&0xFFFFu16.to_le_bytes());

    let (next, records, truncated) = parse_buffer(&buf);
    assert_eq!(next, 700);
    assert_eq!(records.len(), 1, "out-of-bounds name must not parse");
    assert_eq!(String::from_utf16(&records[0].name).unwrap(), "good.txt");
    assert!(truncated, "a dropped record must not be silent");
}

#[test]
fn foreign_major_versions_are_skipped_between_v2_records() {
    // A V3-shaped record (128-bit IDs, ReFS — out of MVP scope) sandwiched
    // between V2 records: skipped by version, both neighbors parsed.
    let mut buf = usn_buffer(
        800,
        &[RecSpec {
            usn: 100,
            frn: frn(70),
            parent_frn: frn(5),
            reason: reason::FILE_CREATE | reason::CLOSE,
            attributes: ARCHIVE,
            name: "first.txt",
        }],
    );
    // Minimal fake V3 record: RecordLength 80, MajorVersion 3.
    let mut v3 = vec![0u8; 80];
    v3[0..4].copy_from_slice(&80u32.to_le_bytes());
    v3[4..6].copy_from_slice(&3u16.to_le_bytes());
    buf.extend_from_slice(&v3);
    encode_record_v2(
        &mut buf,
        &RecSpec {
            usn: 300,
            frn: frn(71),
            parent_frn: frn(5),
            reason: reason::FILE_CREATE | reason::CLOSE,
            attributes: ARCHIVE,
            name: "second.txt",
        },
    );

    let (next, records, truncated) = parse_buffer(&buf);
    assert!(!truncated);
    assert_eq!(next, 800);
    let names: Vec<String> = records
        .iter()
        .map(|r| String::from_utf16(&r.name).unwrap())
        .collect();
    assert_eq!(names, ["first.txt", "second.txt"]);
}

// ── Scenario: compaction mid-stream ──────────────────────────────────────

#[test]
fn replay_continues_correctly_across_a_mid_stream_compaction() {
    let mut idx = base_index();
    // Batch 1: a rename (tombstone + pool garbage) and a create.
    replay(
        &mut idx,
        100,
        &[
            RecSpec {
                usn: 10,
                frn: frn(11),
                parent_frn: frn(10),
                reason: reason::RENAME_NEW_NAME | reason::CLOSE,
                attributes: ARCHIVE,
                name: "Renamed.TXT",
            },
            RecSpec {
                usn: 20,
                frn: frn(30),
                parent_frn: frn(20),
                reason: reason::FILE_CREATE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "fresh.log",
            },
        ],
        &no_stats(),
    );
    assert!(idx.len() > idx.live_len(), "the rename left a tombstone");

    // Compact exactly where the volume thread would: between batches.
    idx = idx.compacted();
    assert_eq!(idx.len(), idx.live_len());
    assert_eq!(path_of(&idx, 11), r"C:\docs\Renamed.TXT");

    // Batch 2 runs against the remapped index: rename the fresh file,
    // delete the renamed one, create another under docs.
    replay(
        &mut idx,
        200,
        &[
            RecSpec {
                usn: 30,
                frn: frn(30),
                parent_frn: frn(20),
                reason: reason::RENAME_NEW_NAME | reason::CLOSE,
                attributes: ARCHIVE,
                name: "fresh2.log",
            },
            RecSpec {
                usn: 40,
                frn: frn(11),
                parent_frn: frn(10),
                reason: reason::FILE_DELETE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "Renamed.TXT",
            },
            RecSpec {
                usn: 50,
                frn: frn(40),
                parent_frn: frn(10),
                reason: reason::FILE_CREATE | reason::CLOSE,
                attributes: ARCHIVE,
                name: "new_note.md",
            },
        ],
        &no_stats(),
    );

    assert_eq!(path_of(&idx, 30), r"C:\archive\fresh2.log");
    assert_eq!(path_of(&idx, 40), r"C:\docs\new_note.md");
    assert!(
        idx.entry_by_record(11).is_none(),
        "deleted after compaction"
    );
    let names = live_names(&idx);
    assert!(names.contains(&"fresh2.log".to_string()));
    assert!(!names.contains(&"fresh.log".to_string()));
}
