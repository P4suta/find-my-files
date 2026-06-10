//! Reduce a journal batch to per-FRN final operations and apply them to the
//! index. Reason flags are aggregated per FRN first (a rename storm touching
//! one file collapses to a single upsert — docs/ARCHITECTURE.md), then ops
//! run in first-touch order so that `mkdir a; touch a\b` resolves parents.

use rustc_hash::FxHashMap;

use super::records::{UsnRecord, reason};
use crate::index::{RawEntry, VolumeIndex, masked};

/// Size/mtime lookup for created/changed files. The USN record carries
/// neither (RESEARCH.md), so the live session asks the volume; replay tests
/// inject canned values.
pub trait StatFetcher {
    fn stat(&self, frn: u64) -> Option<(u64, i64)>;
}

/// Fetcher that never answers — entries keep carried-over (or zero) values.
pub struct NullStatFetcher;
impl StatFetcher for NullStatFetcher {
    fn stat(&self, _frn: u64) -> Option<(u64, i64)> {
        None
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct BatchStats {
    pub created_or_renamed: u32,
    pub deleted: u32,
    pub stat_updated: u32,
    pub ignored: u32,
}

struct Agg {
    reasons: u32,
    /// Index into the batch of the latest record for this FRN (carries the
    /// final name/parent/attributes).
    last: usize,
}

const STAT_REASONS: u32 = reason::DATA_OVERWRITE
    | reason::DATA_EXTEND
    | reason::DATA_TRUNCATION
    | reason::BASIC_INFO_CHANGE;

/// Apply one journal batch. Bumps the content generation exactly once.
pub fn apply_batch(
    idx: &mut VolumeIndex,
    records: &[UsnRecord],
    fetch: &dyn StatFetcher,
) -> BatchStats {
    let mut stats = BatchStats::default();
    let mut order: Vec<u64> = Vec::new();
    let mut agg: FxHashMap<u64, Agg> = FxHashMap::default();

    for (i, r) in records.iter().enumerate() {
        let key = masked(r.frn);
        match agg.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let a = e.get_mut();
                a.reasons |= r.reason;
                a.last = i;
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Agg {
                    reasons: r.reason,
                    last: i,
                });
                order.push(key);
            }
        }
    }

    let first_new = idx.len() as u32;
    for key in order {
        let a = &agg[&key];
        let last = &records[a.last];

        if a.reasons & reason::FILE_DELETE != 0 {
            if idx.delete(key).is_some() {
                stats.deleted += 1;
            } else {
                stats.ignored += 1;
            }
        } else if a.reasons & (reason::FILE_CREATE | reason::RENAME_NEW_NAME) != 0 {
            // Directory rename/move must keep the EntryId stable (children
            // point at it) — handled in place. Files go tombstone+new.
            let existing = idx.entry_by_record(key);
            if let Some(old) = existing
                && idx.is_dir(old)
                && last.is_dir()
            {
                idx.rename_dir_in_place(key, &last.name, masked(last.parent_frn));
                stats.created_or_renamed += 1;
                continue;
            }
            // Carry size/mtime over from the previous entry when the volume
            // can't answer (file already gone, or replay without fixtures).
            let carried = existing.map(|id| (idx.size(id), idx.mtime(id)));
            let (size, mtime) = fetch.stat(last.frn).or(carried).unwrap_or((0, 0));
            idx.upsert(&RawEntry {
                record: key,
                parent_record: last.parent_frn,
                frn: last.frn,
                name_utf16: &last.name,
                is_dir: last.is_dir(),
                is_reparse: last.is_reparse(),
                size,
                mtime,
            });
            stats.created_or_renamed += 1;
        } else if a.reasons & STAT_REASONS != 0 {
            if let Some((size, mtime)) = fetch.stat(last.frn) {
                if idx.update_stat(key, size, mtime).is_some() {
                    stats.stat_updated += 1;
                } else {
                    stats.ignored += 1;
                }
            } else {
                stats.ignored += 1;
            }
        } else {
            stats.ignored += 1;
        }
    }

    idx.merge_new_into_permutations(first_new);
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{SortKey, VolumeIndexBuilder};

    fn rec(frn: u64, parent: u64, reason: u32, attrs: u32, name: &str) -> UsnRecord {
        UsnRecord {
            usn: 0,
            frn,
            parent_frn: parent,
            reason,
            attributes: attrs,
            name: name.encode_utf16().collect(),
        }
    }

    fn base_index() -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let docs: Vec<u16> = "docs".encode_utf16().collect();
        let note: Vec<u16> = "note.txt".encode_utf16().collect();
        b.push(RawEntry {
            record: 10,
            parent_record: 5,
            frn: (1 << 48) | 10,
            name_utf16: &docs,
            is_dir: true,
            is_reparse: false,
            size: 0,
            mtime: 0,
        });
        b.push(RawEntry {
            record: 11,
            parent_record: 10,
            frn: (1 << 48) | 11,
            name_utf16: &note,
            is_dir: false,
            is_reparse: false,
            size: 100,
            mtime: 7,
        });
        b.finish()
    }

    fn path_of(idx: &VolumeIndex, record: u64) -> String {
        let id = idx.entry_by_record(record).unwrap();
        let mut p = Vec::new();
        idx.append_path(id, &mut p);
        String::from_utf8(p).unwrap()
    }

    struct Fixed(u64, i64);
    impl StatFetcher for Fixed {
        fn stat(&self, _frn: u64) -> Option<(u64, i64)> {
            Some((self.0, self.1))
        }
    }

    #[test]
    fn create_in_new_dir_within_one_batch() {
        let mut idx = base_index();
        let batch = [
            rec(20, 5, reason::FILE_CREATE | reason::CLOSE, 0x10, "src"),
            rec(21, 20, reason::FILE_CREATE | reason::CLOSE, 0x20, "main.rs"),
        ];
        let s = apply_batch(&mut idx, &batch, &Fixed(42, 9));
        assert_eq!(s.created_or_renamed, 2);
        assert_eq!(path_of(&idx, 21), r"C:\src\main.rs");
        let id = idx.entry_by_record(21).unwrap();
        assert_eq!((idx.size(id), idx.mtime(id)), (42, 9));
    }

    #[test]
    fn rename_storm_collapses_to_final_name() {
        let mut idx = base_index();
        let batch = [
            rec(11, 10, reason::RENAME_OLD_NAME, 0x20, "note.txt"),
            rec(11, 10, reason::RENAME_NEW_NAME, 0x20, "tmp1.txt"),
            rec(11, 10, reason::RENAME_OLD_NAME, 0x20, "tmp1.txt"),
            rec(
                11,
                10,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x20,
                "final.txt",
            ),
        ];
        let s = apply_batch(&mut idx, &batch, &NullStatFetcher);
        assert_eq!(s.created_or_renamed, 1);
        assert_eq!(path_of(&idx, 11), r"C:\docs\final.txt");
        // Carried over size/mtime survive a rename without a fetcher.
        let id = idx.entry_by_record(11).unwrap();
        assert_eq!((idx.size(id), idx.mtime(id)), (100, 7));
    }

    #[test]
    fn move_to_other_dir_updates_child_paths() {
        let mut idx = base_index();
        let batch = [
            rec(20, 5, reason::FILE_CREATE | reason::CLOSE, 0x10, "archive"),
            rec(
                10,
                20,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x10,
                "docs",
            ),
        ];
        apply_batch(&mut idx, &batch, &NullStatFetcher);
        // docs moved under archive; note.txt's lazy path follows.
        assert_eq!(path_of(&idx, 11), r"C:\archive\docs\note.txt");
    }

    #[test]
    fn create_then_delete_in_one_batch_is_a_delete() {
        let mut idx = base_index();
        let n = idx.live_len();
        let batch = [
            rec(30, 5, reason::FILE_CREATE, 0x20, "ghost.tmp"),
            rec(
                30,
                5,
                reason::FILE_DELETE | reason::CLOSE,
                0x20,
                "ghost.tmp",
            ),
        ];
        let s = apply_batch(&mut idx, &batch, &NullStatFetcher);
        assert_eq!(s.deleted, 0); // never existed in the index
        assert_eq!(s.ignored, 1);
        assert_eq!(idx.live_len(), n);
    }

    #[test]
    fn stat_update_changes_size_and_mtime() {
        let mut idx = base_index();
        let batch = [rec(
            11,
            10,
            reason::DATA_EXTEND | reason::CLOSE,
            0x20,
            "note.txt",
        )];
        let s = apply_batch(&mut idx, &batch, &Fixed(5000, 99));
        assert_eq!(s.stat_updated, 1);
        let id = idx.entry_by_record(11).unwrap();
        assert_eq!((idx.size(id), idx.mtime(id)), (5000, 99));
    }

    #[test]
    fn delete_removes_from_results_and_generation_bumps() {
        let mut idx = base_index();
        let g0 = idx.content_generation();
        let batch = [rec(
            11,
            10,
            reason::FILE_DELETE | reason::CLOSE,
            0x20,
            "note.txt",
        )];
        let s = apply_batch(&mut idx, &batch, &NullStatFetcher);
        assert_eq!(s.deleted, 1);
        assert!(idx.entry_by_record(11).is_none());
        assert_eq!(idx.content_generation(), g0 + 1);
    }

    #[test]
    fn renamed_entry_lands_sorted_in_permutation() {
        let mut idx = base_index();
        let batch = [rec(
            11,
            10,
            reason::RENAME_NEW_NAME | reason::CLOSE,
            0x20,
            "aaa_first.txt",
        )];
        apply_batch(&mut idx, &batch, &NullStatFetcher);
        let perm = idx.permutation(SortKey::Name);
        let live: Vec<&[u8]> = perm
            .iter()
            .filter(|&&id| idx.is_live(id))
            .map(|&id| idx.lower_name(id))
            .collect();
        let mut sorted = live.clone();
        sorted.sort();
        assert_eq!(live, sorted);
    }
}
