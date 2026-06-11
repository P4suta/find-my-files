//! Query execution. Each AND group is driven by a single SIMD sweep over the
//! contiguous name pool (pool-scan / prefix / suffix drivers) that yields a
//! sparse candidate list; residual matchers then verify only those
//! candidates. Groups without a usable literal fall back to a chunked
//! full scan, and the empty query walks the permutation directly. Results
//! materialize as O(1)-pageable, sort-ordered id arrays
//! (docs/ARCHITECTURE.md「クエリ時マテリアライズ」+ perf plan Workstream B).

use rayon::prelude::*;

use super::QueryOptions;
use super::compile::{CTerm, CompiledQuery, Driver};
use super::matchers::{EvalCtx, terms_match, terms_match_iter};
use super::memo::{DirPaths, OffsetTable};
use super::sweep::driver_candidates;
use crate::index::{EntryId, VolumeIndex};

/// 65536 entries per parallel task for full scans.
const CHUNK: usize = 1 << 16;

pub struct SearchResult {
    /// Matching entries in the requested sort order.
    pub ids: Vec<EntryId>,
    pub content_generation: u64,
    pub structural_generation: u64,
}

/// Per-volume stage timings for [`crate::metrics::QueryTrace`].
#[derive(Debug, Default, Clone)]
pub struct SearchMetrics {
    pub driver: String,
    pub memo_us: u64,
    pub scan_us: u64,
    pub materialize_us: u64,
    pub entries_scanned: u64,
    pub excluded_skipped: u64,
}

// ── Search ──────────────────────────────────────────────────────────────

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
    let cached_memo;
    let empty_memo;
    let memo: &DirPaths = if q.needs_paths() {
        cached_memo = idx.cached_derived_or_update(|prev| match prev {
            Some(p) => DirPaths::extend_from(idx, p),
            None => DirPaths::build(idx),
        });
        &cached_memo
    } else {
        empty_memo = DirPaths::empty();
        &empty_memo
    };
    let needs_table = q
        .groups
        .iter()
        .any(|g| !matches!(g.driver, Driver::FullScan | Driver::MatchAll));
    let table: Option<std::sync::Arc<OffsetTable>> = if needs_table {
        Some(idx.cached_derived_or_update(|prev| match prev {
            Some(p) => OffsetTable::extend_from(idx, p),
            None => OffsetTable::build(idx),
        }))
    } else {
        None
    };
    metrics.memo_us = stage.lap();

    let n = idx.len();
    let mut bitmap: Vec<u64> = vec![0u64; n.div_ceil(64)];

    for group in &q.groups {
        match &group.driver {
            Driver::MatchAll => {
                // No terms in this OR-branch → every live entry matches.
                full_scan_group(
                    idx,
                    memo,
                    &group.terms,
                    skip_excluded,
                    &mut bitmap,
                    &mut metrics,
                );
            }
            Driver::FullScan => {
                full_scan_group(
                    idx,
                    memo,
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
                if group.terms.is_empty() {
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
                                .filter(|&id| terms_match(idx, memo, &mut ctx, &group.terms, id))
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

    let cached_memo;
    let empty_memo;
    let memo: &DirPaths = if q.needs_paths() {
        cached_memo = idx.cached_derived_or_update(|prev| match prev {
            Some(p) => DirPaths::extend_from(idx, p),
            None => DirPaths::build(idx),
        });
        &cached_memo
    } else {
        empty_memo = DirPaths::empty();
        &empty_memo
    };
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
                            .any(|g| terms_match_iter(idx, memo, &mut ctx, g.all_terms(), id))
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
    memo: &DirPaths,
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
/// parallel chunks, order preserved by concatenation.
fn materialize_filtered(
    idx: &VolumeIndex,
    opt: &QueryOptions,
    keep: impl Fn(EntryId) -> bool + Sync,
) -> Vec<EntryId> {
    let perm = idx.permutation(opt.sort);
    // Fine-grained chunks: at 2^17 a 1M-entry walk only fans out 8 ways and
    // the walk becomes the latency floor for every query.
    const MAT_CHUNK: usize = 1 << 14;
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
        // Everything semantics: `docs\` matches entries *under* the folder.
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

    #[test]
    fn date_filter_utc() {
        let idx = sample();
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
            "ort",
            "tab",
            "ba",
            "ファイル",
        ];
        let mut rng = Rng(42);
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let mut names_made: Vec<String> = Vec::new();
        for i in 0..500u64 {
            let mut name = String::new();
            for _ in 0..(1 + rng.next() % 4) {
                name.push_str(fragments[(rng.next() % fragments.len() as u64) as usize]);
            }
            name.push_str(&format!("_{i}"));
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                record: 100 + i,
                parent_record: 5,
                frn: (1 << 48) | (100 + i),
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
                record: 100 + i,
                parent_record: 5,
                frn: (1 << 48) | (100 + i),
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
