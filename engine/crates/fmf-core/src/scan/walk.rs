//! Non-elevated folder-walk scanner for scope mode (ADR-0024).
//!
//! The privileged path streams the whole $MFT (`scan_volume`); this one walks
//! only the roots the user can read, with no admin and no raw volume handle.
//! It builds the same [`VolumeIndex`] the $MFT scanner does, so the query
//! layer and the worker are untouched — the only differences are the record
//! key (a synthetic path hash, `walk_id`) and the change source (the Phase 2
//! `WatcherJournalSource` instead of the USN journal).
//!
//! Layout trick (no index-format change): each configured root is pushed as a
//! detached top-level entry whose *name is its absolute base path*; the synthetic
//! `VolumeIndex::ROOT` (slot 0) carries an empty name that
//! `append_parent_path` skips. So a root's children reconstruct as
//! `C:\Users\me\Documents\sub\file.txt` with no special path code.
//!
//! Memory safety: enumeration is pure safe `std::fs` — on Windows `read_dir`
//! caches each `WIN32_FIND_DATAW`, so `DirEntry::metadata()` serves
//! size/mtime/attributes from that cache with no extra syscall and no `unsafe`.

use std::path::PathBuf;
use std::time::Instant;

use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;

use crate::index::{Frn, RawEntry, VolumeIndex, VolumeIndexBuilder};
use crate::wtf8::push_wtf8_pair;

use super::ScanStats;
use super::walk_id::path_record;

// Raw `dwFileAttributes` bits we classify on (windows_sys mirrors these; kept
// local so the hot enumeration loop has no import churn).
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_REPARSE_POINT: u32 = 0x400;

/// Depth cap, kept under `append_parent_path`'s 128-deep chain buffer so a
/// pathological tree can never produce a truncated path silently.
const MAX_DEPTH: u32 = 100;

/// The synthetic record of `VolumeIndex::ROOT` (slot 0). Roots attach here;
/// the value 0 is reserved (a real path colliding with it is a ~2⁻⁴⁸ event
/// that only shadows slot 0, which carries no name).
const SCOPE_ROOT_RECORD: u64 = 0;

/// One directory queued for enumeration: its on-disk path plus the data its
/// children need — the parent's folded path (to extend, not re-fold) and the
/// parent's record (the children's `parent_frn`).
struct Pending {
    path: PathBuf,
    folded: Vec<u8>,
    record: u64,
    depth: u32,
}

/// Walk `roots` (absolute base paths) and build a queryable [`VolumeIndex`].
///
/// Infallible by design (落ちない): an unreadable root or directory is counted
/// in [`ScanStats::walk_read_errors`] and skipped, never propagated. The
/// worker maps that count to a counter + warn at its single mapping point.
#[must_use]
pub fn walk_scan(roots: &[String]) -> (VolumeIndex, ScanStats) {
    let t0 = Instant::now();
    let mut stats = ScanStats {
        volume: "scope".to_string(),
        ..Default::default()
    };
    let mut b = VolumeIndexBuilder::new("", SCOPE_ROOT_RECORD);

    // Reused across every entry so the walk allocates O(depth), not O(entries).
    let mut units: Vec<u16> = Vec::new();
    let mut name_buf: Vec<u8> = Vec::new();
    let mut lower_buf: Vec<u8> = Vec::new();
    let mut stack: Vec<Pending> = Vec::new();

    for root in roots {
        let path = PathBuf::from(root);
        let md = match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => m,
            _ => {
                stats.walk_read_errors += 1;
                continue;
            }
        };
        units.clear();
        units.extend(path.as_os_str().encode_wide());
        name_buf.clear();
        lower_buf.clear();
        push_wtf8_pair(&units, &mut name_buf, &mut lower_buf);
        let record = path_record(&lower_buf);
        let attrs = md.file_attributes();
        // The root's *name* is its whole absolute base path; with slot 0's
        // empty name skipped by append_parent_path, children resolve to a
        // correct absolute path with no path-code change.
        b.push(RawEntry {
            parent_frn: Frn(SCOPE_ROOT_RECORD),
            frn: Frn(record),
            name_utf16: &units,
            is_dir: true,
            is_reparse: attrs & ATTR_REPARSE_POINT != 0,
            is_hidden: attrs & ATTR_HIDDEN != 0,
            is_system: attrs & ATTR_SYSTEM != 0,
            size: md.file_size(),
            mtime: md.last_write_time() as i64,
        });
        stats.dirs += 1;
        stack.push(Pending {
            path,
            folded: lower_buf.clone(),
            record,
            depth: 0,
        });
    }

    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur.path) else {
            stats.walk_read_errors += 1;
            continue;
        };
        for entry in rd {
            let Ok(entry) = entry else {
                stats.walk_read_errors += 1;
                continue;
            };
            // No extra syscall on Windows: served from the cached find data,
            // and (like symlink_metadata) it does not traverse reparse points.
            let Ok(md) = entry.metadata() else {
                stats.walk_read_errors += 1;
                continue;
            };
            let attrs = md.file_attributes();
            let is_dir = attrs & ATTR_DIRECTORY != 0;
            let is_reparse = attrs & ATTR_REPARSE_POINT != 0;

            let name = entry.file_name();
            units.clear();
            units.extend(name.encode_wide());
            name_buf.clear();
            lower_buf.clear();
            // lower_buf = folded name; folding is per-char and length-
            // preserving (ADR-0003), so folded(parent)+"\"+folded(name) equals
            // folded(full path) — the watcher recomputes the same key.
            push_wtf8_pair(&units, &mut name_buf, &mut lower_buf);
            let mut folded_child = cur.folded.clone();
            folded_child.push(b'\\');
            folded_child.extend_from_slice(&lower_buf);
            let record = path_record(&folded_child);

            b.push(RawEntry {
                parent_frn: Frn(cur.record),
                frn: Frn(record),
                name_utf16: &units,
                is_dir,
                is_reparse,
                is_hidden: attrs & ATTR_HIDDEN != 0,
                is_system: attrs & ATTR_SYSTEM != 0,
                size: md.file_size(),
                mtime: md.last_write_time() as i64,
            });
            if is_dir {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }

            // Descend into real directories only — never follow reparse points
            // (junctions/symlinks loop and can escape the root).
            if is_dir && !is_reparse {
                if cur.depth + 1 < MAX_DEPTH {
                    stack.push(Pending {
                        path: entry.path(),
                        folded: folded_child,
                        record,
                        depth: cur.depth + 1,
                    });
                } else {
                    stats.walk_depth_truncated += 1;
                }
            }
        }
    }

    stats.elapsed_walk_ms = t0.elapsed().as_millis() as u64;
    stats.walk_dirs = stats.dirs;
    stats.walk_files = stats.files;
    let (idx, finish) = b.finish_timed();
    stats.elapsed_build_ms = finish.build_ms;
    stats.elapsed_sort_ms = finish.sort_ms;
    stats.elapsed_total_ms = t0.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = crate::mft::peak_working_set();
    (idx, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real on-disk tree under a unique temp dir, removed on drop.
    struct Tree(PathBuf);
    impl Tree {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("fmf-walk-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp tree root");
            Self(dir)
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn folded(path: &str) -> Vec<u8> {
        let units: Vec<u16> = std::path::Path::new(path)
            .as_os_str()
            .encode_wide()
            .collect();
        let (mut n, mut l) = (Vec::new(), Vec::new());
        push_wtf8_pair(&units, &mut n, &mut l);
        l
    }

    #[test]
    fn walk_builds_queryable_index_with_correct_paths() {
        let tree = Tree::new("paths");
        let root = tree.0.join("Documents");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("alpha.txt"), b"a").unwrap();
        std::fs::write(root.join("sub").join("beta.log"), b"bb").unwrap();

        let root_str = root.to_str().unwrap().to_string();
        let (idx, stats) = walk_scan(std::slice::from_ref(&root_str));

        // Root + sub (2 dirs) and alpha.txt + beta.log (2 files).
        assert_eq!(stats.dirs, 2, "dirs");
        assert_eq!(stats.files, 2, "files");
        assert_eq!(stats.walk_read_errors, 0, "no read errors");

        // The deep file reconstructs to its true absolute path.
        let beta_path = format!("{root_str}\\sub\\beta.log");
        let id = idx
            .entry_by_record(path_record(&folded(&beta_path)))
            .expect("beta.log indexed");
        let mut out = Vec::new();
        idx.append_path(id, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), beta_path);

        // Parent linkage resolved across walk order: beta.log → sub → root.
        let sub_id = idx
            .entry_by_record(path_record(&folded(&format!("{root_str}\\sub"))))
            .expect("sub indexed");
        assert_eq!(idx.parent(id), sub_id);
        let root_id = idx
            .entry_by_record(path_record(&folded(&root_str)))
            .expect("root indexed");
        assert_eq!(idx.parent(sub_id), root_id);
        assert!(idx.is_dir(sub_id));
        assert_eq!(idx.size(id), 2);
    }

    #[test]
    fn multiple_roots_share_one_index() {
        let tree = Tree::new("multiroot");
        let a = tree.0.join("RootA");
        let b = tree.0.join("RootB");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("one.txt"), b"1").unwrap();
        std::fs::write(b.join("two.txt"), b"2").unwrap();

        let (idx, stats) = walk_scan(&[
            a.to_str().unwrap().to_string(),
            b.to_str().unwrap().to_string(),
        ]);
        assert_eq!(stats.files, 2, "one file per root");

        for (root, name) in [(&a, "one.txt"), (&b, "two.txt")] {
            let p = format!("{}\\{name}", root.to_str().unwrap());
            let id = idx
                .entry_by_record(path_record(&folded(&p)))
                .unwrap_or_else(|| panic!("{p} indexed"));
            let mut out = Vec::new();
            idx.append_path(id, &mut out);
            assert_eq!(String::from_utf8(out).unwrap(), p);
        }
    }

    #[test]
    fn missing_root_is_counted_not_fatal() {
        let (idx, stats) = walk_scan(&["Z:\\does\\not\\exist-fmf".to_string()]);
        assert_eq!(stats.walk_read_errors, 1);
        // Only the synthetic ROOT slot exists; no real entries.
        assert_eq!(stats.files, 0);
        assert_eq!(stats.dirs, 0);
        assert_eq!(idx.live_len(), 1, "just the empty synthetic root");
    }
}
