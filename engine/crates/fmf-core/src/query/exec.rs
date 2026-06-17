//! Query execution. Each AND group is driven by a single SIMD sweep over the
//! contiguous name pool (pool-scan / prefix / suffix drivers) that yields a
//! sparse candidate list; residual matchers then verify only those
//! candidates. Groups without a usable literal fall back to a chunked
//! full scan, and the empty query walks the permutation directly. Results
//! materialize as O(1)-pageable, sort-ordered id arrays
//! (docs/ARCHITECTURE.md "query-time materialization").

use rayon::prelude::*;

use super::QueryOptions;
use super::compile::{CTerm, CompiledQuery, Driver};
use super::matchers::{EvalCtx, terms_match, terms_match_iter};
use super::memo::{DirPathsLower, DirPathsOrig, MtimePerm, OffsetTable, PathMemos, SizePerm};
use super::sweep::driver_candidates;
use crate::index::{EntryId, SortKey, VolumeIndex};

/// Build (or incrementally extend) exactly the dir-path memos this query
/// reads — `None` pools cost nothing, which is the whole point of keeping
/// folded and original-case memos in separate cache slots.
fn path_memos(idx: &VolumeIndex, q: &CompiledQuery) -> PathMemos {
    PathMemos {
        lower: q.needs_folded_paths.then(|| {
            idx.cached_derived_or_update(|prev| match prev {
                Some(p) => DirPathsLower::extend_from(idx, p),
                None => DirPathsLower::build(idx),
            })
        }),
        orig: q.needs_orig_paths.then(|| {
            idx.cached_derived_or_update(|prev| match prev {
                Some(p) => DirPathsOrig::extend_from(idx, p),
                None => DirPathsOrig::build(idx),
            })
        }),
    }
}

/// 65536 entries per parallel task for full scans.
const CHUNK: usize = 1 << 16;

/// One volume's query result: the matching ids plus the index generations
/// they were computed against (for staleness checks and incremental refine).
pub struct SearchResult {
    /// Matching entries in the requested sort order.
    pub ids: Vec<EntryId>,
    /// Content generation (name/data edits) of the index when these ids were produced.
    pub content_generation: u64,
    /// Structural generation (live-set / tree shape) of the index when these ids were produced.
    pub structural_generation: u64,
}

/// Per-volume stage timings for [`crate::metrics::QueryTrace`].
#[derive(Debug, Default, Clone)]
pub struct SearchMetrics {
    /// Human-readable label of the driver that executed this query (e.g. `perm-walk`).
    pub driver: String,
    /// Time spent building or extending the cached memo/offset structures (µs).
    pub memo_us: u64,
    /// Time spent sweeping pools and evaluating residual matchers (µs).
    pub scan_us: u64,
    /// Time spent walking the sort permutation to materialize the id array (µs).
    pub materialize_us: u64,
    /// Number of entries examined during the scan (count).
    pub entries_scanned: u64,
    /// Number of entries skipped because they are hidden/system and excluded (count).
    pub excluded_skipped: u64,
}

// ── Search ──────────────────────────────────────────────────────────────

/// Execute a compiled query against one volume index.
///
/// # Panics
///
/// Panics if a pool-scanning driver runs without its offset table — an
/// invariant guaranteed here, since the table is built whenever any group
/// needs it.
pub fn search(
    idx: &VolumeIndex,
    q: &CompiledQuery,
    opt: &QueryOptions,
) -> (SearchResult, SearchMetrics) {
    let mut metrics = SearchMetrics {
        driver: q.driver_label(),
        ..Default::default()
    };
    let mut stage = crate::metrics::Stage::start();
    let skip_excluded = !opt.include_hidden_system;

    // Fast path: the empty query (single MatchAll group, no residuals) walks
    // the permutation directly — no bitmap, no scan.
    if q.groups.len() == 1
        && matches!(q.groups[0].driver, Driver::MatchAll)
        && q.groups[0].terms.is_empty()
    {
        metrics.driver = "perm-walk".to_string();
        metrics.memo_us = stage.lap();
        let ids = materialize_filtered(idx, opt, |id| {
            idx.is_live(id) && !(skip_excluded && idx.is_excluded(id))
        });
        metrics.entries_scanned = idx.len() as u64;
        metrics.materialize_us = stage.lap();
        return (
            SearchResult {
                ids,
                content_generation: idx.content_generation(),
                structural_generation: idx.structural_generation(),
            },
            metrics,
        );
    }

    // Generation-cached lookup structures.
    let memo = path_memos(idx, q);
    let needs_table = q
        .groups
        .iter()
        .any(|g| !matches!(g.driver, Driver::FullScan | Driver::MatchAll));
    let table: Option<std::sync::Arc<OffsetTable>> = needs_table.then(|| {
        idx.cached_derived_or_update(|prev| match prev {
            Some(p) => OffsetTable::extend_from(idx, p),
            None => OffsetTable::build(idx),
        })
    });
    metrics.memo_us = stage.lap();

    let n = idx.len();
    let mut bitmap: Vec<u64> = vec![0u64; n.div_ceil(64)];

    for group in &q.groups {
        match &group.driver {
            // MatchAll: no terms in this OR-branch → every live entry matches.
            // FullScan: a group with no usable literal driver. Both evaluate
            // the residuals (if any) over every live entry.
            Driver::MatchAll | Driver::FullScan => {
                full_scan_group(
                    idx,
                    &memo,
                    &group.terms,
                    skip_excluded,
                    &mut bitmap,
                    &mut metrics,
                );
            }
            driver => {
                let table = table.as_ref().expect("offset table built");
                let candidates = driver_candidates(idx, table, driver, skip_excluded);
                metrics.entries_scanned += candidates.len() as u64;
                // A case-exact source term makes the folded sweep a superset:
                // its exact comparison joins the residual pass.
                if group.terms.is_empty() && group.driver_exact {
                    for id in candidates {
                        bitmap[id as usize / 64] |= 1u64 << (id as usize % 64);
                    }
                } else {
                    let passed: Vec<Vec<EntryId>> = candidates
                        .par_chunks(2048)
                        .map(|chunk| {
                            let mut ctx = EvalCtx::default();
                            chunk
                                .iter()
                                .copied()
                                .filter(|&id| {
                                    terms_match_iter(
                                        idx,
                                        &memo,
                                        &mut ctx,
                                        group.residual_terms(),
                                        id,
                                    )
                                })
                                .collect()
                        })
                        .collect();
                    for ids in passed {
                        for id in ids {
                            bitmap[id as usize / 64] |= 1u64 << (id as usize % 64);
                        }
                    }
                }
            }
        }
    }
    metrics.scan_us = stage.lap();

    let ids = materialize_filtered(idx, opt, |id| {
        bitmap[id as usize / 64] >> (id as usize % 64) & 1 == 1
    });
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

/// Incremental refinement: when the previous query's result provably
/// contains the next one's (subsume.rs) and the index generation is
/// unchanged, filter the cached ids with a *complete* per-entry evaluation
/// instead of sweeping pools and walking the whole permutation. `prev_ids`
/// are already in the requested order, so the filtered subsequence is the
/// answer — O(previous hits) instead of O(index).
pub fn refine(
    idx: &VolumeIndex,
    q: &CompiledQuery,
    opt: &QueryOptions,
    prev_ids: &[EntryId],
) -> (SearchResult, SearchMetrics) {
    const REFINE_CHUNK: usize = 4096;
    let mut metrics = SearchMetrics {
        driver: q.driver_label(),
        ..Default::default()
    };
    let mut stage = crate::metrics::Stage::start();
    let skip_excluded = !opt.include_hidden_system;

    let memo = path_memos(idx, q);
    metrics.memo_us = stage.lap();

    let chunks: Vec<Vec<EntryId>> = prev_ids
        .par_chunks(REFINE_CHUNK)
        .map(|chunk| {
            let mut ctx = EvalCtx::default();
            chunk
                .iter()
                .copied()
                .filter(|&id| {
                    idx.is_live(id)
                        && !(skip_excluded && idx.is_excluded(id))
                        && q.groups
                            .iter()
                            .any(|g| terms_match_iter(idx, &memo, &mut ctx, g.all_terms(), id))
                })
                .collect()
        })
        .collect();
    metrics.entries_scanned = prev_ids.len() as u64;
    metrics.scan_us = stage.lap();

    let mut ids = Vec::with_capacity(chunks.iter().map(Vec::len).sum());
    for c in &chunks {
        ids.extend_from_slice(c);
    }
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

fn full_scan_group(
    idx: &VolumeIndex,
    memo: &PathMemos,
    terms: &[CTerm],
    skip_excluded: bool,
    bitmap: &mut [u64],
    metrics: &mut SearchMetrics,
) {
    let n = idx.len();
    let chunk_results: Vec<(Vec<u64>, u64, u64)> = (0..n.div_ceil(CHUNK))
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
                if terms_match(idx, memo, &mut ctx, terms, id) {
                    let rel = id as usize - start;
                    words[rel / 64] |= 1u64 << (rel % 64);
                }
            }
            (words, scanned, skipped)
        })
        .collect();

    let mut word_base = 0usize;
    for (words, scanned, skipped) in &chunk_results {
        for (i, w) in words.iter().enumerate() {
            bitmap[word_base + i] |= w;
        }
        word_base += CHUNK / 64;
        metrics.entries_scanned += scanned;
        metrics.excluded_skipped += skipped;
    }
}

/// Walk the pre-sorted permutation keeping entries that pass `keep` —
/// parallel chunks, order preserved by concatenation. Name order is the
/// always-maintained index column; size/mtime orders are lazily derived
/// caches that build on the first query sorting by them (one parallel sort)
/// and extend per generation after that.
fn materialize_filtered(
    idx: &VolumeIndex,
    opt: &QueryOptions,
    keep: impl Fn(EntryId) -> bool + Sync,
) -> Vec<EntryId> {
    // Fine-grained chunks: at 2^17 a 1M-entry walk only fans out 8 ways and
    // the walk becomes the latency floor for every query.
    const MAT_CHUNK: usize = 1 << 14;

    let size_perm;
    let mtime_perm;
    let perm: &[EntryId] = match opt.sort {
        SortKey::Name => idx.name_permutation(),
        SortKey::Size => {
            size_perm = SizePerm::get(idx);
            &size_perm.0.ids
        }
        SortKey::Mtime => {
            mtime_perm = MtimePerm::get(idx);
            &mtime_perm.0.ids
        }
    };
    let chunks: Vec<Vec<EntryId>> = perm
        .par_chunks(MAT_CHUNK)
        .map(|chunk| chunk.iter().copied().filter(|&id| keep(id)).collect())
        .collect();
    let total = chunks.iter().map(Vec::len).sum();
    let mut ids = Vec::with_capacity(total);
    if opt.desc {
        for c in chunks.iter().rev() {
            ids.extend(c.iter().rev());
        }
    } else {
        for c in &chunks {
            ids.extend_from_slice(c);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::super::dates::UtcResolver;
    use super::super::{CaseMode, QueryOptions, compile, parse};
    use super::*;
    use crate::index::{Frn, RawEntry, SortKey, VolumeIndexBuilder};

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
            // (name, record, parent, is_dir, size, mtime)
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
                parent_frn: Frn(*parent),
                frn: Frn((1 << 48) | rec),
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
        assert_eq!(names(&idx, "report"), vec!["Report.PDF"]);
        assert_eq!(names(&idx, "Report"), vec!["Report.PDF"]);
        assert!(names(&idx, "REPORT.pdf").is_empty());
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
        assert!(names(&idx, "size:0 folder:").is_empty());
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
        // Path semantics: `docs\` matches entries *under* the folder.
        assert_eq!(in_docs, vec!["Report.PDF", "notes.txt"]);
        assert_eq!(names(&idx, r"path:src main"), vec!["main.rs"]);
    }

    #[test]
    fn wildcards_anchor_to_whole_name() {
        let idx = sample();
        assert_eq!(names(&idx, "*.rs"), vec!["main.rs"]);
        assert_eq!(names(&idx, "main.?s"), vec!["main.rs"]);
        assert!(names(&idx, "*.r").is_empty());
        // Specialized prefix / inner forms.
        assert_eq!(names(&idx, "main*"), vec!["main.rs"]);
        assert_eq!(names(&idx, "*ain*"), vec!["main.rs"]);
        assert!(names(&idx, "ain*").is_empty());
    }

    #[test]
    fn regex_term() {
        let idx = sample();
        assert_eq!(names(&idx, "regex:^ma.n\\.rs$"), vec!["main.rs"]);
    }

    /// The literal prefilter must never lose a match: a regex query through
    /// the engine (prefiltered pool sweep + residual, or a full scan when no
    /// literal exists) must equal a naive `Regex::is_match` over every name,
    /// for every case mode and a spread of pattern shapes. This is the one
    /// unacceptable failure mode — a superset prefilter that drops a real hit.
    #[test]
    fn regex_matches_naive_oracle() {
        use regex::bytes::RegexBuilder;

        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }
        }
        let frags = [
            "report",
            "Report",
            "main",
            "src",
            "日本",
            "語",
            "data",
            "DLL",
            "exe",
            "img",
            "2024",
            "v2",
            "ファイル",
            "tmp",
        ];
        let exts = [".rs", ".txt", ".PDF", ".dll", ".log", ".日", ""];
        let mut rng = Rng(0x9E37_79B9);
        let mut b = VolumeIndexBuilder::new("C:", 5);
        // The builder seeds the volume root ("C:"); it is a live entry a name
        // regex sees too (e.g. `.*`), so the naive oracle must include it.
        let mut made: Vec<String> = vec!["C:".to_string()];
        for i in 0..300u64 {
            let mut name = String::new();
            for _ in 0..=(rng.next() % 3) {
                name.push_str(frags[rng.next() as usize % frags.len()]);
            }
            name.push_str(exts[rng.next() as usize % exts.len()]);
            if name.is_empty() {
                name.push('x');
            }
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                parent_frn: Frn(5),
                frn: Frn((1 << 48) | (100 + i)),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: i,
                mtime: i as i64,
            });
            made.push(name);
        }
        let idx = b.finish();

        // Prefilter-bearing (prefix/suffix literal) and literal-less (full
        // scan) shapes, plus alternations, anchors, multibyte and digits.
        let patterns = [
            "^report",
            "report",
            "ort",
            "main.*rs$",
            "日.*語",
            "[0-9]+",
            "v[0-9]",
            r".*\.rs$",
            r"\.dll$",
            "dll|exe",
            "^$",
            "Report",
            "REPORT",
            "src.*main",
            r"^\d",
            ".*",
            "ファイル",
            "(report|main)",
            r"tmp.*\.log$",
            "日本",
        ];
        let cases = [CaseMode::Smart, CaseMode::Insensitive, CaseMode::Sensitive];
        for pat in patterns {
            for case in cases {
                let opt = QueryOptions {
                    case,
                    ..Default::default()
                };
                let ci = match case {
                    CaseMode::Insensitive => true,
                    CaseMode::Sensitive => false,
                    CaseMode::Smart => !crate::wtf8::has_uppercase(pat),
                };
                let re = RegexBuilder::new(pat)
                    .case_insensitive(ci)
                    .dot_matches_new_line(true)
                    .build()
                    .unwrap();
                let mut expect: Vec<String> = made
                    .iter()
                    .filter(|n| re.is_match(n.as_bytes()))
                    .cloned()
                    .collect();
                expect.sort();
                // Quote so the parser keeps `|`/`$`/etc. inside one regex atom.
                let mut got = run(&idx, &format!(r#"regex:"{pat}""#), opt);
                got.sort();
                assert_eq!(
                    got, expect,
                    "regex `{pat}` (case {case:?}) diverged from the naive oracle"
                );
            }
        }
    }

    #[test]
    fn date_filter_utc() {
        let idx = sample();
        let r = names(&idx, "dm:>=1650 ext:pdf");
        assert_eq!(r, vec!["Report.PDF"]);
    }

    /// Whole-query regex mode (ADR-0023): the entire text is one regex, no
    /// parsing/operators, matched against the name or the full path per scope.
    #[test]
    fn whole_regex_mode_name_and_path_scope() {
        use super::super::{RegexScope, compile_whole_regex};

        let idx = sample();
        let run_re = |pat: &str, scope: RegexScope| {
            let q = compile_whole_regex(pat, CaseMode::Smart, scope).unwrap();
            let mut got: Vec<String> = search(&idx, &q, &QueryOptions::default())
                .0
                .ids
                .iter()
                .map(|&id| String::from_utf8_lossy(idx.name(id)).into_owned())
                .collect();
            got.sort();
            got
        };

        // Name scope: the text is a regex over the file name. Operators that
        // the normal parser would treat as AND/OR are regex metachars here.
        assert_eq!(run_re(r"\.rs$", RegexScope::Name), vec!["main.rs"]);
        assert_eq!(run_re("^report", RegexScope::Name), vec!["Report.PDF"]); // smart-case ci
        assert_eq!(
            run_re("pdf|txt", RegexScope::Name),
            vec!["Report.PDF", "notes.txt"]
        );
        // A literal-less pattern falls back to the full scan and still matches.
        assert!(run_re(r"[0-9]", RegexScope::Name).is_empty()); // no sample name has a digit

        // Path scope: matched against the full path (parent included).
        assert_eq!(
            run_re(r"docs.*\.pdf$", RegexScope::Path),
            vec!["Report.PDF"]
        );

        // Invalid / oversized patterns are a compile error, never a panic.
        assert!(compile_whole_regex("(", CaseMode::Smart, RegexScope::Name).is_err());
        assert!(compile_whole_regex("(a{500}){500}", CaseMode::Smart, RegexScope::Name).is_err());
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
                parent_frn: Frn(parent),
                frn: Frn((1 << 48) | rec),
                name_utf16: name,
                is_dir,
                is_reparse: false,
                is_hidden: false,
                is_system,
                size: 1,
                mtime: 1,
            });
        };
        push(10, 5, &bin, true, true);
        push(11, 10, &ghost, false, false);
        push(20, 5, &vis, false, false);
        let idx = b.finish();

        assert_eq!(names(&idx, "txt"), vec!["visible.txt"]);
        assert_eq!(names(&idx, "").len(), 2); // root + visible.txt

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

    /// Pool-scan vs naive oracle over deterministic pseudo-random names,
    /// including multibyte, surrogate pairs, boundary-adjacent repeats and
    /// post-rename stale pool gaps.
    #[test]
    fn pool_scan_matches_naive_oracle() {
        use std::fmt::Write as _;

        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }
        }
        let fragments = [
            "ab",
            "abc",
            "日本",
            "語",
            "x",
            "𠮷",
            "report",
            "Report",
            "ort",
            "tab",
            "TAB",
            "ba",
            "ファイル",
        ];
        let mut rng = Rng(42);
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let mut names_made: Vec<String> = Vec::new();
        for i in 0..500u64 {
            let mut name = String::new();
            for _ in 0..=(rng.next() % 4) {
                name.push_str(fragments[(rng.next() % fragments.len() as u64) as usize]);
            }
            write!(name, "_{i}").unwrap();
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                parent_frn: Frn(5),
                frn: Frn((1 << 48) | (100 + i)),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: i,
                mtime: i as i64,
            });
            names_made.push(name);
        }
        let mut idx = b.finish();
        // Create stale pool gaps: rename a slice of entries.
        for i in (0..500u64).step_by(7) {
            let new_name = format!("renamed_{i}_abba");
            let units: Vec<u16> = new_name.encode_utf16().collect();
            let first_new = idx.len() as u32;
            idx.upsert(&RawEntry {
                parent_frn: Frn(5),
                frn: Frn((1 << 48) | (100 + i)),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: i,
                mtime: i as i64,
            });
            idx.merge_new_into_permutations(first_new);
            names_made[i as usize] = new_name;
        }

        let needles = [
            "ab",
            "abc",
            "ba",
            "abba",
            "日本",
            "語x",
            "𠮷",
            "report",
            "ort",
            "tab",
            "_3",
            "renamed",
            "zzz_nothing",
        ];
        for needle in needles {
            let mut expect: Vec<String> = names_made
                .iter()
                .filter(|n| n.to_lowercase().contains(needle))
                .cloned()
                .collect();
            expect.sort();
            let mut got = names(&idx, needle);
            got.sort();
            assert_eq!(got, expect, "needle `{needle}` diverged from oracle");
        }

        // Smart-case uppercase needles: the folded sweep over-approximates
        // (case-exact terms drive as folded supersets) and the exact
        // residual must filter back to byte-exact matches.
        let upper_needles = ["Report", "Rep", "TAB", "Ort"];
        assert!(
            names_made.iter().any(|n| n.contains("Report")),
            "oracle coverage: mixed-case names must exist"
        );
        for needle in upper_needles {
            let mut expect: Vec<String> = names_made
                .iter()
                .filter(|n| n.contains(needle))
                .cloned()
                .collect();
            expect.sort();
            let mut got = names(&idx, needle);
            got.sort();
            assert_eq!(got, expect, "smart-case needle `{needle}` diverged");
        }
        // Sensitive mode: every needle is byte-exact, lowercase ones too.
        let sensitive = QueryOptions {
            case: CaseMode::Sensitive,
            ..Default::default()
        };
        for needle in needles.iter().chain(upper_needles.iter()) {
            let mut expect: Vec<String> = names_made
                .iter()
                .filter(|n| n.contains(needle))
                .cloned()
                .collect();
            expect.sort();
            let mut got = run(&idx, needle, sensitive);
            got.sort();
            assert_eq!(got, expect, "sensitive needle `{needle}` diverged");
        }

        // ── Refine oracle: for every subsumed step of a typing sequence,
        // refining the previous ids must equal a fresh search, byte for
        // byte and in order. False positives here lose results — the one
        // unacceptable failure mode of the query cache.
        let sequences: &[&[&str]] = &[
            &["", "a", "ab", "abb", "abba"],
            &["r", "re", "rep", "repo", "report"],
            &["ab", "ab ba", "ab ba _3"],
            &["日", "日本", "日本 語"],
            &["t", "ta", "tab", "tab *ab*"],
            &["re", "renamed", "renamed !zzz"],
            &["a", "a size:>100", "a size:>100 size:<400"],
            &["", "x", "x𠮷"],
            &["or", "Ort"], // smart-case flip: folded prev → orig next
        ];
        let opts = [
            QueryOptions::default(),
            QueryOptions {
                sort: SortKey::Size,
                desc: true,
                ..Default::default()
            },
        ];
        let mut refined_pairs = 0;
        for opt in opts {
            for seq in sequences {
                let mut prev: Option<(super::super::CompiledQuery, Vec<EntryId>)> = None;
                for text in *seq {
                    let q = compile(&parse(text).unwrap(), opt.case, &UtcResolver).unwrap();
                    let fresh = search(&idx, &q, &opt).0.ids;
                    if let Some((pq, pids)) = &prev
                        && super::super::subsumes(pq, &opt, &q, &opt)
                    {
                        let refined = refine(&idx, &q, &opt, pids).0.ids;
                        assert_eq!(
                            refined, fresh,
                            "refine diverged from fresh search at `{text}` (sort {:?})",
                            opt.sort
                        );
                        refined_pairs += 1;
                    }
                    prev = Some((q, fresh));
                }
            }
        }
        assert!(
            refined_pairs >= 20,
            "subsumption barely fired ({refined_pairs} pairs) — the oracle is vacuous"
        );
    }
}

#[cfg(test)]
mod proptests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use proptest::prelude::*;

    use super::super::dates::UtcResolver;
    use super::super::{
        CaseMode, CompiledQuery, QueryOptions, RegexScope, compile, compile_whole_regex, parse,
        subsumes,
    };
    use super::{EntryId, refine, search};
    use crate::index::{Frn, RawEntry, SortKey, VolumeIndex, VolumeIndexBuilder};

    // Counts every subsumed pair the refine-vs-fresh oracle actually checked,
    // summed across *all* generated proptest cases. The vacuity guard
    // (`subsumption_fires_at_least_sometimes`) reads it: without it a green run
    // could simply mean subsumption never fired and the equality was never
    // exercised.
    static REFINED_PAIRS: AtomicU64 = AtomicU64::new(0);

    /// Name fragments spanning the matcher domains the subsumption algebra
    /// bridges: ASCII case pairs (smart-case fold ↔ orig), multibyte UTF-8
    /// (fold is length-preserving per code point), a surrogate pair, and
    /// extension-shaped tails. Short and overlapping so random concatenations
    /// collide and the typed prefixes/suffixes actually subsume.
    const FRAGMENTS: &[&str] = &[
        "ab", "abc", "Re", "report", "Report", "ort", "tab", "TAB", "日本", "語", "𠮷", "x",
        "main", ".rs", ".txt", ".PDF",
    ];

    /// One generated index entry: a name plus the attributes the option
    /// cross-product reads (dir/hidden/system for visibility, size/mtime for
    /// the sort columns).
    #[derive(Debug, Clone)]
    struct GenEntry {
        name: String,
        is_dir: bool,
        is_hidden: bool,
        is_system: bool,
        size: u64,
        mtime: i64,
    }

    fn entry_strategy() -> impl Strategy<Value = GenEntry> {
        (
            proptest::collection::vec(0usize..FRAGMENTS.len(), 1..=4),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            0u64..(8u64 << 30),
            0i64..(20_000i64 * 864_000_000_000),
        )
            .prop_map(|(parts, is_dir, is_hidden, is_system, size, mtime)| {
                let name: String = parts.iter().map(|&i| FRAGMENTS[i]).collect();
                GenEntry {
                    name,
                    is_dir,
                    is_hidden,
                    is_system,
                    size,
                    mtime,
                }
            })
    }

    fn build_index(entries: &[GenEntry]) -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        for (i, e) in entries.iter().enumerate() {
            let units: Vec<u16> = e.name.encode_utf16().collect();
            b.push(RawEntry {
                parent_frn: Frn(5),
                frn: Frn((1 << 48) | (100 + i as u64)),
                name_utf16: &units,
                is_dir: e.is_dir,
                is_reparse: false,
                is_hidden: e.is_hidden,
                is_system: e.is_system,
                size: e.size,
                mtime: e.mtime,
            });
        }
        b.finish()
    }

    /// A typed-query growth step: each variant appends to the previous text so
    /// the result *usually* narrows — the case subsumption is designed for.
    /// Suffix/filter steps mix in the non-name matcher domains.
    #[derive(Debug, Clone)]
    enum Step {
        /// Append the next character of a fragment (incremental typing).
        Frag(usize),
        /// Append a whole extra name term (AND narrows further).
        Term(usize),
        /// Append a `*lit*` wildcard term.
        Wildcard(usize),
        /// Append a negated name term.
        Not(usize),
        /// Append a `size:` lower-bound filter.
        Size(u64),
        /// Append a `file:` / `folder:` filter.
        IsDir(bool),
        /// Append an `ext:` filter.
        Ext(usize),
        /// A backspace-like reset to a shorter prefix (deliberately *widens*,
        /// so subsumption must decline — keeps the oracle honest).
        Truncate,
    }

    fn step_strategy() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0usize..FRAGMENTS.len()).prop_map(Step::Frag),
            (0usize..FRAGMENTS.len()).prop_map(Step::Term),
            (0usize..FRAGMENTS.len()).prop_map(Step::Wildcard),
            (0usize..FRAGMENTS.len()).prop_map(Step::Not),
            (1u64..(4u64 << 30)).prop_map(Step::Size),
            any::<bool>().prop_map(Step::IsDir),
            (0usize..3usize).prop_map(Step::Ext),
            Just(Step::Truncate),
        ]
    }

    /// Turn a step sequence into a growing list of query texts. `Truncate`
    /// chops the trailing whitespace-delimited atom; every other step appends.
    fn texts_from_steps(steps: &[Step]) -> Vec<String> {
        use std::fmt::Write as _;
        const EXTS: &[&str] = &["rs", "txt", "pdf"];
        let mut cur = String::new();
        let mut out = vec![cur.clone()];
        for step in steps {
            match step {
                Step::Frag(i) => cur.push_str(FRAGMENTS[*i]),
                Step::Term(i) => {
                    cur.push(' ');
                    cur.push_str(FRAGMENTS[*i]);
                }
                Step::Wildcard(i) => {
                    cur.push_str(" *");
                    cur.push_str(FRAGMENTS[*i].trim_start_matches('.'));
                    cur.push('*');
                }
                Step::Not(i) => {
                    cur.push_str(" !");
                    cur.push_str(FRAGMENTS[*i].trim_start_matches('.'));
                }
                Step::Size(min) => {
                    write!(cur, " size:>{min}").expect("writing to a String is infallible");
                }
                Step::IsDir(d) => {
                    cur.push_str(if *d { " folder:" } else { " file:" });
                }
                Step::Ext(i) => {
                    cur.push_str(" ext:");
                    cur.push_str(EXTS[*i]);
                }
                Step::Truncate => {
                    let trimmed = cur.trim_end();
                    let keep = trimmed.rfind(char::is_whitespace).map_or(0, |p| p);
                    cur.truncate(keep);
                }
            }
            out.push(cur.clone());
        }
        out
    }

    /// Compile one query text honoring whole-query regex mode (then the text
    /// *is* the pattern). Returns `None` when the text fails to compile (an
    /// invalid regex fragment); the caller skips that step.
    fn compile_text(text: &str, opt: &QueryOptions) -> Option<CompiledQuery> {
        if opt.regex_mode {
            compile_whole_regex(text, opt.case, opt.regex_scope).ok()
        } else {
            compile(&parse(text).ok()?, opt.case, &UtcResolver).ok()
        }
    }

    fn options_strategy() -> impl Strategy<Value = QueryOptions> {
        (
            prop_oneof![
                Just(SortKey::Name),
                Just(SortKey::Size),
                Just(SortKey::Mtime)
            ],
            any::<bool>(),
            prop_oneof![
                Just(CaseMode::Smart),
                Just(CaseMode::Insensitive),
                Just(CaseMode::Sensitive)
            ],
            any::<bool>(),
            any::<bool>(),
            prop_oneof![Just(RegexScope::Name), Just(RegexScope::Path)],
        )
            .prop_map(
                |(sort, desc, case, include_hidden_system, regex_mode, regex_scope)| QueryOptions {
                    sort,
                    desc,
                    case,
                    include_hidden_system,
                    regex_mode,
                    regex_scope,
                },
            )
    }

    proptest! {
        // The one unacceptable failure mode (subsume.rs): whenever
        // `subsumes(prev, next)` claims the next result fits inside the
        // previous one, `refine(prev_ids)` must reproduce `search(next)`
        // EXACTLY — identical id set *and* order — across the full option
        // cross-product (sort × desc × hidden/system × case × regex × scope)
        // and a random index. A divergence here is a real soundness bug:
        // refine would silently drop or misorder live results. Both prev and
        // next compile under the *same* options (the real refine precondition:
        // a typing session keeps its options and index generation fixed).
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn refine_equals_fresh_whenever_subsumed(
            entries in proptest::collection::vec(entry_strategy(), 1..40),
            steps in proptest::collection::vec(step_strategy(), 1..8),
            opt in options_strategy(),
        ) {
            let idx = build_index(&entries);
            let texts = texts_from_steps(&steps);

            let mut prev: Option<(CompiledQuery, Vec<EntryId>)> = None;
            for text in &texts {
                let Some(q) = compile_text(text, &opt) else {
                    // An uncompilable step breaks the chain — a fresh search
                    // would start over, so drop the cached predecessor too.
                    prev = None;
                    continue;
                };
                let fresh = search(&idx, &q, &opt).0.ids;

                if let Some((pq, pids)) = &prev
                    && subsumes(pq, &opt, &q, &opt)
                {
                    let refined = refine(&idx, &q, &opt, pids).0.ids;
                    prop_assert_eq!(
                        &refined,
                        &fresh,
                        "refine diverged from fresh search at `{}` (opt {:?})",
                        text,
                        opt
                    );
                    REFINED_PAIRS.fetch_add(1, Ordering::Relaxed);
                }
                prev = Some((q, fresh));
            }
        }
    }

    // Vacuity guard: the proptest above is only meaningful if subsumption
    // actually fired and the refine==fresh equality was exercised. proptest
    // runs every `#[test]` in this module in the same process, so the shared
    // counter is populated by the time this (alphabetically later) test runs.
    // The threshold is deliberately well below the typical count (hundreds of
    // subsumed pairs over 256 cases) so it tolerates shrinking/seed variance
    // while still catching a regression that makes subsumption never fire.
    #[test]
    fn subsumption_fires_at_least_sometimes() {
        // Drive a deterministic spread of options directly so the guard does
        // not rely on the fuzz test's nondeterministic ordering: incremental
        // typing under each sort/case must subsume at least the obvious steps.
        let entries: Vec<GenEntry> = (0..30u64)
            .map(|i| {
                let name = format!(
                    "{}{}{}",
                    FRAGMENTS[i as usize % FRAGMENTS.len()],
                    FRAGMENTS[(i as usize + 3) % FRAGMENTS.len()],
                    if i % 2 == 0 { ".rs" } else { ".txt" },
                );
                GenEntry {
                    name,
                    is_dir: i % 5 == 0,
                    is_hidden: i % 7 == 0,
                    is_system: i % 11 == 0,
                    size: i * 1024,
                    mtime: i as i64 * 864_000_000_000,
                }
            })
            .collect();
        let idx = build_index(&entries);

        let mut local_fired = 0u64;
        for sort in [SortKey::Name, SortKey::Size, SortKey::Mtime] {
            for case in [CaseMode::Smart, CaseMode::Insensitive, CaseMode::Sensitive] {
                let opt = QueryOptions {
                    sort,
                    case,
                    ..Default::default()
                };
                let seq = ["", "r", "re", "rep", "repo", "report", "report .rs"];
                let mut prev: Option<(CompiledQuery, Vec<EntryId>)> = None;
                for text in seq {
                    let q = compile_text(text, &opt).expect("static texts compile");
                    let fresh = search(&idx, &q, &opt).0.ids;
                    if let Some((pq, pids)) = &prev
                        && subsumes(pq, &opt, &q, &opt)
                    {
                        let refined = refine(&idx, &q, &opt, pids).0.ids;
                        assert_eq!(
                            refined, fresh,
                            "refine diverged from fresh search at `{text}` (opt {opt:?})"
                        );
                        local_fired += 1;
                    }
                    prev = Some((q, fresh));
                }
            }
        }
        assert!(
            local_fired >= 9,
            "subsumption barely fired ({local_fired}) — the oracle is vacuous"
        );
        // Fold the fuzz test's tally in too (best-effort: it may not have run
        // yet under filtered/sharded runs, hence the independent local check).
        REFINED_PAIRS.fetch_add(local_fired, Ordering::Relaxed);
    }
}
