//! Parallel query execution: chunked scan into a match bitmap, then one walk
//! over the pre-sorted permutation to materialize ids in final sort order
//! (docs/ARCHITECTURE.md「クエリ時マテリアライズ」). Page reads afterwards are
//! O(1) slices of the returned id array.

use rayon::prelude::*;

use super::QueryOptions;
use super::compile::{CompiledQuery, Matcher};
use crate::index::{EntryId, VolumeIndex};

/// 65536 entries per parallel task = 1024 bitmap words; keeps chunk bitmaps
/// word-aligned so assembly is pure concatenation.
const CHUNK: usize = 1 << 16;

pub struct SearchResult {
    /// Matching entries in the requested sort order.
    pub ids: Vec<EntryId>,
    pub content_generation: u64,
    pub structural_generation: u64,
}

/// Per-volume stage timings for [`crate::metrics::QueryTrace`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchMetrics {
    pub memo_us: u64,
    pub scan_us: u64,
    pub materialize_us: u64,
    pub entries_scanned: u64,
    pub excluded_skipped: u64,
}

/// Memoized full paths for every directory (only built when the query
/// contains path terms). Entry paths are `memo[parent] + name`.
struct DirPaths {
    lower: Vec<Option<Box<[u8]>>>,
    orig: Vec<Option<Box<[u8]>>>,
}

impl DirPaths {
    fn build(idx: &VolumeIndex, want_lower: bool, want_orig: bool) -> Self {
        let n = idx.len();
        let mut memo = DirPaths {
            lower: vec![None; if want_lower { n } else { 0 }],
            orig: vec![None; if want_orig { n } else { 0 }],
        };
        let mut stack: Vec<EntryId> = Vec::new();
        for id in 0..n as u32 {
            if idx.is_dir(id) {
                memo.ensure(idx, id, &mut stack);
            }
        }
        memo
    }

    fn ensure(&mut self, idx: &VolumeIndex, dir: EntryId, stack: &mut Vec<EntryId>) {
        stack.clear();
        let mut cur = dir;
        loop {
            let missing = (!self.lower.is_empty() && self.lower[cur as usize].is_none())
                || (!self.orig.is_empty() && self.orig[cur as usize].is_none());
            if !missing {
                break;
            }
            stack.push(cur);
            if cur == VolumeIndex::ROOT {
                break;
            }
            cur = idx.parent(cur);
        }
        while let Some(d) = stack.pop() {
            if !self.lower.is_empty() && self.lower[d as usize].is_none() {
                let mut p = if d == VolumeIndex::ROOT {
                    Vec::new()
                } else {
                    self.lower[idx.parent(d) as usize]
                        .as_deref()
                        .unwrap_or(&[])
                        .to_vec()
                };
                p.extend_from_slice(idx.lower_name(d));
                p.push(b'\\');
                self.lower[d as usize] = Some(p.into_boxed_slice());
            }
            if !self.orig.is_empty() && self.orig[d as usize].is_none() {
                let mut p = if d == VolumeIndex::ROOT {
                    Vec::new()
                } else {
                    self.orig[idx.parent(d) as usize]
                        .as_deref()
                        .unwrap_or(&[])
                        .to_vec()
                };
                p.extend_from_slice(idx.name(d));
                p.push(b'\\');
                self.orig[d as usize] = Some(p.into_boxed_slice());
            }
        }
    }

    #[inline]
    fn parent_prefix(pool: &[Option<Box<[u8]>>], parent: EntryId) -> &[u8] {
        pool.get(parent as usize)
            .and_then(|p| p.as_deref())
            .unwrap_or(&[])
    }
}

/// Per-thread scratch: the entry's full path, built at most once per entry
/// per variant, only when a path matcher is actually reached.
#[derive(Default)]
struct EvalCtx {
    lower_path: Vec<u8>,
    orig_path: Vec<u8>,
    lower_built: bool,
    orig_built: bool,
}

impl EvalCtx {
    #[inline]
    fn reset(&mut self) {
        self.lower_built = false;
        self.orig_built = false;
    }

    #[inline]
    fn lower_path<'a>(&'a mut self, idx: &VolumeIndex, memo: &DirPaths, id: EntryId) -> &'a [u8] {
        if !self.lower_built {
            self.lower_path.clear();
            if id != VolumeIndex::ROOT {
                self.lower_path
                    .extend_from_slice(DirPaths::parent_prefix(&memo.lower, idx.parent(id)));
            }
            self.lower_path.extend_from_slice(idx.lower_name(id));
            self.lower_built = true;
        }
        &self.lower_path
    }

    #[inline]
    fn orig_path<'a>(&'a mut self, idx: &VolumeIndex, memo: &DirPaths, id: EntryId) -> &'a [u8] {
        if !self.orig_built {
            self.orig_path.clear();
            if id != VolumeIndex::ROOT {
                self.orig_path
                    .extend_from_slice(DirPaths::parent_prefix(&memo.orig, idx.parent(id)));
            }
            self.orig_path.extend_from_slice(idx.name(id));
            self.orig_built = true;
        }
        &self.orig_path
    }
}

#[inline]
fn eval(idx: &VolumeIndex, memo: &DirPaths, ctx: &mut EvalCtx, m: &Matcher, id: EntryId) -> bool {
    match m {
        Matcher::True => true,
        Matcher::Size { min, max } => !idx.is_dir(id) && (*min..=*max).contains(&idx.size(id)),
        Matcher::Mtime { min, max } => (*min..=*max).contains(&idx.mtime(id)),
        Matcher::IsDir(d) => idx.is_dir(id) == *d,
        Matcher::Ext { exts } => {
            let lower = idx.lower_name(id);
            match memchr::memrchr(b'.', lower) {
                Some(p) if !idx.is_dir(id) => {
                    let ext = &lower[p + 1..];
                    exts.iter().any(|e| e.as_slice() == ext)
                }
                _ => false,
            }
        }
        Matcher::NameSub { finder, folded } => {
            let hay = if *folded {
                idx.lower_name(id)
            } else {
                idx.name(id)
            };
            finder.find(hay).is_some()
        }
        Matcher::NameRegex { re } => re.is_match(idx.name(id)),
        Matcher::PathSub { finder, folded } => {
            let hay = if *folded {
                ctx.lower_path(idx, memo, id)
            } else {
                ctx.orig_path(idx, memo, id)
            };
            finder.find(hay).is_some()
        }
        Matcher::PathRegex { re } => re.is_match(ctx.orig_path(idx, memo, id)),
    }
}

#[inline]
fn matches(
    idx: &VolumeIndex,
    memo: &DirPaths,
    ctx: &mut EvalCtx,
    q: &CompiledQuery,
    id: EntryId,
) -> bool {
    ctx.reset();
    'groups: for g in &q.groups {
        for t in g {
            if eval(idx, memo, ctx, &t.matcher, id) == t.negated {
                continue 'groups;
            }
        }
        return true;
    }
    false
}

pub fn search(
    idx: &VolumeIndex,
    q: &CompiledQuery,
    opt: &QueryOptions,
) -> (SearchResult, SearchMetrics) {
    let mut metrics = SearchMetrics::default();
    let mut stage = crate::metrics::Stage::start();

    // The dir-path memo depends only on index content, not the query, so it
    // is cached on the index per content generation (a cold build costs
    // ~150ms on 300k dirs — too slow to repeat per keystroke). Both pools are
    // built so every path query hits the same cache entry.
    let cached;
    let empty;
    let memo: &DirPaths = if q.needs_paths() {
        cached = idx.cached_path_memo(|| DirPaths::build(idx, true, true));
        &cached
    } else {
        empty = DirPaths {
            lower: Vec::new(),
            orig: Vec::new(),
        };
        &empty
    };
    metrics.memo_us = stage.lap();

    let skip_excluded = !opt.include_hidden_system;
    let n = idx.len();
    let chunk_bitmaps: Vec<(Vec<u64>, u64, u64)> = (0..n.div_ceil(CHUNK))
        .into_par_iter()
        .map(|ci| {
            let start = ci * CHUNK;
            let end = (start + CHUNK).min(n);
            let mut words = vec![0u64; (end - start).div_ceil(64)];
            let mut ctx = EvalCtx::default();
            let mut scanned = 0u64;
            let mut skipped = 0u64;
            for id in start..end {
                let id = id as EntryId;
                if !idx.is_live(id) {
                    continue;
                }
                if skip_excluded && idx.is_excluded(id) {
                    skipped += 1;
                    continue;
                }
                scanned += 1;
                if matches(idx, memo, &mut ctx, q, id) {
                    let rel = id as usize - start;
                    words[rel / 64] |= 1u64 << (rel % 64);
                }
            }
            (words, scanned, skipped)
        })
        .collect();

    let mut bitmap: Vec<u64> = Vec::with_capacity(n.div_ceil(64));
    for (w, scanned, skipped) in &chunk_bitmaps {
        bitmap.extend_from_slice(w);
        metrics.entries_scanned += scanned;
        metrics.excluded_skipped += skipped;
    }
    metrics.scan_us = stage.lap();

    let ids = materialize(idx, &bitmap, opt);
    metrics.materialize_us = stage.lap();

    (
        SearchResult {
            ids,
            content_generation: idx.content_generation(),
            structural_generation: idx.structural_generation(),
        },
        metrics,
    )
}

/// Walk the pre-sorted permutation and keep bitmap hits — parallel chunks,
/// order preserved by concatenation.
fn materialize(idx: &VolumeIndex, bitmap: &[u64], opt: &QueryOptions) -> Vec<EntryId> {
    let perm = idx.permutation(opt.sort);
    let hit = |id: EntryId| bitmap[id as usize / 64] >> (id as usize % 64) & 1 == 1;

    const MAT_CHUNK: usize = 1 << 17;
    let mut chunks: Vec<Vec<EntryId>> = perm
        .par_chunks(MAT_CHUNK)
        .map(|chunk| chunk.iter().copied().filter(|&id| hit(id)).collect())
        .collect();
    if opt.desc {
        chunks.reverse();
        let mut ids = Vec::with_capacity(chunks.iter().map(Vec::len).sum());
        for c in &chunks {
            ids.extend(c.iter().rev());
        }
        ids
    } else {
        let mut ids = Vec::with_capacity(chunks.iter().map(Vec::len).sum());
        for c in &chunks {
            ids.extend_from_slice(c);
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::super::dates::UtcResolver;
    use super::super::{CaseMode, QueryOptions, compile, parse};
    use super::*;
    use crate::index::{RawEntry, SortKey, VolumeIndexBuilder};

    fn run(idx: &VolumeIndex, query: &str, opt: QueryOptions) -> Vec<String> {
        let ast = parse(query).unwrap();
        let q = compile(&ast, opt.case, &UtcResolver).unwrap();
        search(idx, &q, &opt)
            .0
            .ids
            .iter()
            .map(|&id| String::from_utf8_lossy(idx.name(id)).into_owned())
            .collect()
    }

    fn names(idx: &VolumeIndex, query: &str) -> Vec<String> {
        run(idx, query, QueryOptions::default())
    }

    /// C:\ ├─ Docs\(dir) │ ├─ Report.PDF │ └─ notes.txt ├─ src\(dir) │ └─ main.rs └─ big.BIN
    fn sample() -> VolumeIndex {
        let day = 864_000_000_000i64; // FILETIME ticks per day
        let entries: &[(&str, u64, u64, bool, u64, i64)] = &[
            // (name, record, parent, is_dir, size, mtime_days_since_1601)
            ("Docs", 10, 5, true, 0, 100 * day),
            ("Report.PDF", 11, 10, false, 2 << 20, 19_000 * day),
            ("notes.txt", 12, 10, false, 512, 19_100 * day),
            ("src", 20, 5, true, 0, 100 * day),
            ("main.rs", 21, 20, false, 4096, 19_200 * day),
            ("big.BIN", 30, 5, false, 3 << 30, 19_300 * day),
        ];
        let mut b = VolumeIndexBuilder::new("C:", 5);
        for (name, rec, parent, is_dir, size, mtime) in entries {
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                record: *rec,
                parent_record: *parent,
                frn: (1 << 48) | rec,
                name_utf16: &units,
                is_dir: *is_dir,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: *size,
                mtime: *mtime,
            });
        }
        b.finish()
    }

    #[test]
    fn substring_smart_case() {
        let idx = sample();
        // lowercase needle → case-insensitive
        assert_eq!(names(&idx, "report"), vec!["Report.PDF"]);
        // uppercase needle → case-sensitive
        assert_eq!(names(&idx, "Report"), vec!["Report.PDF"]);
        assert!(names(&idx, "REPORT.pdf").is_empty());
        // forced insensitive
        let opt = QueryOptions {
            case: CaseMode::Insensitive,
            ..Default::default()
        };
        assert_eq!(run(&idx, "REPORT", opt), vec!["Report.PDF"]);
    }

    #[test]
    fn and_or_not() {
        let idx = sample();
        assert_eq!(names(&idx, "main rs"), vec!["main.rs"]);
        let mut both = names(&idx, "pdf | txt");
        both.sort();
        assert_eq!(both, vec!["Report.PDF", "notes.txt"]);
        assert_eq!(names(&idx, ".txt !notes"), Vec::<String>::new());
    }

    #[test]
    fn ext_filter_case_insensitive_and_files_only() {
        let idx = sample();
        assert_eq!(names(&idx, "ext:pdf"), vec!["Report.PDF"]);
        let mut multi = names(&idx, "ext:pdf;rs");
        multi.sort();
        assert_eq!(multi, vec!["Report.PDF", "main.rs"]);
    }

    #[test]
    fn size_filter_excludes_dirs() {
        let idx = sample();
        assert_eq!(names(&idx, "size:>1gb"), vec!["big.BIN"]);
        assert_eq!(names(&idx, "size:512"), vec!["notes.txt"]);
        assert!(names(&idx, "size:0 folder:").is_empty()); // size never matches dirs
    }

    #[test]
    fn folder_and_file_filters() {
        let idx = sample();
        let mut dirs = names(&idx, "folder:");
        dirs.sort();
        assert_eq!(dirs, vec!["C:", "Docs", "src"]);
        assert_eq!(names(&idx, "folder:doc"), vec!["Docs"]);
    }

    #[test]
    fn path_terms() {
        let idx = sample();
        let mut in_docs = names(&idx, r"docs\");
        in_docs.sort();
        // Everything semantics: `docs\` matches entries *under* the folder;
        // the folder itself (path `C:\Docs`, no trailing slash) does not hit.
        assert_eq!(in_docs, vec!["Report.PDF", "notes.txt"]);
        assert_eq!(names(&idx, r"path:src main"), vec!["main.rs"]);
    }

    #[test]
    fn wildcards_anchor_to_whole_name() {
        let idx = sample();
        assert_eq!(names(&idx, "*.rs"), vec!["main.rs"]);
        assert_eq!(names(&idx, "main.?s"), vec!["main.rs"]);
        assert!(names(&idx, "*.r").is_empty());
    }

    #[test]
    fn regex_term() {
        let idx = sample();
        assert_eq!(names(&idx, "regex:^ma.n\\.rs$"), vec!["main.rs"]);
    }

    #[test]
    fn date_filter_utc() {
        let idx = sample();
        // 19000 days since 1601 ≈ 1653 AD + 52y ≈ year 1653? Not meaningful —
        // use a range query around the stored tick values instead.
        // mtime of Report.PDF = 19000 days since 1601 → year ≈ 1653.
        // Use dm:>1652 to include all three files but not the dirs (100 days).
        let r = names(&idx, "dm:>=1650 ext:pdf");
        assert_eq!(r, vec!["Report.PDF"]);
    }

    #[test]
    fn empty_query_matches_all_live_entries() {
        let idx = sample();
        assert_eq!(names(&idx, "").len(), idx.live_len());
    }

    #[test]
    fn sort_orders() {
        let idx = sample();
        let by_size = run(
            &idx,
            "file:",
            QueryOptions {
                sort: SortKey::Size,
                ..Default::default()
            },
        );
        assert_eq!(
            by_size,
            vec!["notes.txt", "main.rs", "Report.PDF", "big.BIN"]
        );
        let by_size_desc = run(
            &idx,
            "file:",
            QueryOptions {
                sort: SortKey::Size,
                desc: true,
                ..Default::default()
            },
        );
        assert_eq!(
            by_size_desc,
            vec!["big.BIN", "Report.PDF", "main.rs", "notes.txt"]
        );
    }

    #[test]
    fn tombstones_are_excluded() {
        let mut idx = sample();
        idx.delete(11); // Report.PDF
        assert!(names(&idx, "report").is_empty());
    }

    #[test]
    fn hidden_system_excluded_by_default_and_toggleable() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let mk = |name: &str| name.encode_utf16().collect::<Vec<u16>>();
        let (bin, ghost, vis) = (mk("$Recycle.Bin"), mk("ghost.txt"), mk("visible.txt"));
        let mut push = |rec: u64, parent: u64, name: &[u16], is_dir, is_system| {
            b.push(RawEntry {
                record: rec,
                parent_record: parent,
                frn: (1 << 48) | rec,
                name_utf16: name,
                is_dir,
                is_reparse: false,
                is_hidden: false,
                is_system,
                size: 1,
                mtime: 1,
            });
        };
        push(10, 5, &bin, true, true); // system dir
        push(11, 10, &ghost, false, false); // plain file inside it
        push(20, 5, &vis, false, false);
        let idx = b.finish();

        // Default: only the visible file (plus root) shows, even for "".
        assert_eq!(names(&idx, "txt"), vec!["visible.txt"]);
        assert_eq!(names(&idx, "").len(), 2); // root + visible.txt

        // Toggle on: everything is searchable again.
        let all = run(
            &idx,
            "txt",
            QueryOptions {
                include_hidden_system: true,
                ..Default::default()
            },
        );
        let mut all = all;
        all.sort();
        assert_eq!(all, vec!["ghost.txt", "visible.txt"]);
    }
}
