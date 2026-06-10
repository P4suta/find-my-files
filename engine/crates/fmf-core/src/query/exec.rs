//! Query execution. Each AND group is driven by a single SIMD sweep over the
//! contiguous name pool (pool-scan / prefix / suffix drivers) that yields a
//! sparse candidate list; residual matchers then verify only those
//! candidates. Groups without a usable literal fall back to a chunked
//! full scan, and the empty query walks the permutation directly. Results
//! materialize as O(1)-pageable, sort-ordered id arrays
//! (docs/ARCHITECTURE.md「クエリ時マテリアライズ」+ perf plan Workstream B).

use rayon::prelude::*;

use super::QueryOptions;
use super::compile::{CTerm, CompiledQuery, Driver, Matcher};
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

// ── Pool offset table (generation-cached) ───────────────────────────────

/// Sorted (pool offset → entry id) table that maps pool-scan hits back to
/// entries. `name_off` loses monotonicity after renames (new names append),
/// so the sorted copy is rebuilt per content generation via the index's
/// derived-cache slot.
pub(crate) struct OffsetTable {
    offs: Vec<u32>,
    ids: Vec<EntryId>,
}

impl OffsetTable {
    fn build(idx: &VolumeIndex) -> Self {
        let mut pairs: Vec<(u32, EntryId)> = (0..idx.len() as u32)
            .map(|id| (idx.name_off_of(id), id))
            .collect();
        pairs.par_sort_unstable();
        OffsetTable {
            offs: pairs.iter().map(|p| p.0).collect(),
            ids: pairs.iter().map(|p| p.1).collect(),
        }
    }

    fn len(&self) -> usize {
        self.offs.len()
    }
}

// ── Dir-path memo (generation-cached) ───────────────────────────────────

/// Memoized full paths for every directory (only built when the query
/// contains path terms). Entry paths are `memo[parent] + name`.
struct DirPaths {
    lower: Vec<Option<Box<[u8]>>>,
    orig: Vec<Option<Box<[u8]>>>,
}

impl DirPaths {
    /// Level-order parallel build: a directory's path depends only on its
    /// parent's (one level up), so each depth level fans out across cores.
    fn build(idx: &VolumeIndex) -> Self {
        let n = idx.len();
        let mut memo = DirPaths {
            lower: vec![None; n],
            orig: vec![None; n],
        };

        // Depth per directory via memoized chain walks (serial, O(n)).
        let mut depth: Vec<u32> = vec![u32::MAX; n];
        let mut stack: Vec<EntryId> = Vec::new();
        let mut max_depth = 0u32;
        for id in 0..n as u32 {
            if !idx.is_dir(id) {
                continue;
            }
            let mut cur = id;
            stack.clear();
            while depth[cur as usize] == u32::MAX {
                stack.push(cur);
                if cur == VolumeIndex::ROOT {
                    break;
                }
                cur = idx.parent(cur);
                if stack.len() > 4096 {
                    break; // corrupt parent cycle — treat as root-attached
                }
            }
            while let Some(d) = stack.pop() {
                let v = if d == VolumeIndex::ROOT {
                    0
                } else {
                    depth[idx.parent(d) as usize].saturating_add(1)
                };
                depth[d as usize] = v;
                max_depth = max_depth.max(v);
            }
        }

        let mut levels: Vec<Vec<EntryId>> = vec![Vec::new(); max_depth as usize + 1];
        for id in 0..n as u32 {
            if idx.is_dir(id) && depth[id as usize] != u32::MAX {
                levels[depth[id as usize] as usize].push(id);
            }
        }

        type BuiltDir = (EntryId, Box<[u8]>, Box<[u8]>);
        for level in levels {
            let built: Vec<BuiltDir> = level
                .into_par_iter()
                .map(|d| {
                    let (mut lower, mut orig) = if d == VolumeIndex::ROOT {
                        (Vec::new(), Vec::new())
                    } else {
                        let p = idx.parent(d) as usize;
                        (
                            memo.lower[p].as_deref().unwrap_or(&[]).to_vec(),
                            memo.orig[p].as_deref().unwrap_or(&[]).to_vec(),
                        )
                    };
                    lower.extend_from_slice(idx.lower_name(d));
                    lower.push(b'\\');
                    orig.extend_from_slice(idx.name(d));
                    orig.push(b'\\');
                    (d, lower.into_boxed_slice(), orig.into_boxed_slice())
                })
                .collect();
            for (d, lower, orig) in built {
                memo.lower[d as usize] = Some(lower);
                memo.orig[d as usize] = Some(orig);
            }
        }
        memo
    }

    #[inline]
    fn parent_prefix(pool: &[Option<Box<[u8]>>], parent: EntryId) -> &[u8] {
        pool.get(parent as usize)
            .and_then(|p| p.as_deref())
            .unwrap_or(&[])
    }
}

/// Build the generation-cached query accelerators (dir-path memo + pool
/// offset table) ahead of the first query — the engine calls this once a
/// volume turns Ready so no keystroke ever pays the cold cost.
pub(crate) fn prewarm(idx: &VolumeIndex) {
    let _ = idx.cached_derived(|| DirPaths::build(idx));
    let _: std::sync::Arc<OffsetTable> = idx.cached_derived(|| OffsetTable::build(idx));
}

// ── Residual matcher evaluation ─────────────────────────────────────────

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
        Matcher::NamePrefix { bytes, folded } => {
            let hay = if *folded {
                idx.lower_name(id)
            } else {
                idx.name(id)
            };
            hay.starts_with(bytes)
        }
        Matcher::NameSuffix { bytes, folded } => {
            let hay = if *folded {
                idx.lower_name(id)
            } else {
                idx.name(id)
            };
            hay.ends_with(bytes)
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
fn terms_match(
    idx: &VolumeIndex,
    memo: &DirPaths,
    ctx: &mut EvalCtx,
    terms: &[CTerm],
    id: EntryId,
) -> bool {
    ctx.reset();
    for t in terms {
        if eval(idx, memo, ctx, &t.matcher, id) == t.negated {
            return false;
        }
    }
    true
}

// ── Drivers ─────────────────────────────────────────────────────────────

/// Run a pool-sweep driver and return live (and, when filtering, non-
/// excluded) candidate entries. Hits are validated against entry
/// boundaries: pool bytes from renamed-away names ("stale gaps") and
/// matches spanning two names map outside their entry's range and are
/// rejected.
fn driver_candidates(
    idx: &VolumeIndex,
    table: &OffsetTable,
    driver: &Driver,
    skip_excluded: bool,
) -> Vec<EntryId> {
    let folded = match driver {
        Driver::Sub { folded, .. }
        | Driver::Prefix { folded, .. }
        | Driver::Suffixes { folded, .. } => *folded,
        _ => unreachable!("non-sweep driver"),
    };
    let pool: &[u8] = if folded {
        idx.lower_pool_bytes()
    } else {
        idx.name_pool_bytes()
    };
    if table.len() == 0 || pool.is_empty() {
        return Vec::new();
    }

    // Over-split so uneven hit densities still balance across threads.
    let threads = rayon::current_num_threads().max(1) * 4;
    let per = table.len().div_ceil(threads).max(1);
    let ranges: Vec<(usize, usize)> = (0..table.len())
        .step_by(per)
        .map(|s| (s, (s + per).min(table.len())))
        .collect();

    let accept = |id: EntryId| idx.is_live(id) && !(skip_excluded && idx.is_excluded(id));

    let chunks: Vec<Vec<EntryId>> = ranges
        .into_par_iter()
        .map(|(ks, ke)| {
            let pool_start = table.offs[ks] as usize;
            let pool_end = if ke < table.len() {
                table.offs[ke] as usize
            } else {
                pool.len()
            };
            let hay = &pool[pool_start..pool_end];
            let mut out = Vec::new();

            let mut sweep =
                |needle_len: usize,
                 find: &mut dyn FnMut(&[u8]) -> Option<usize>,
                 anchor: &mut dyn FnMut(usize, usize, usize) -> bool| {
                    let mut pos = 0usize;
                    // Hits arrive in increasing pool order, so the entry
                    // cursor advances monotonically — amortized O(1) per hit
                    // instead of a binary search.
                    let mut k = ks;
                    while pos < hay.len() {
                        let Some(rel) = find(&hay[pos..]) else { break };
                        let hit = pool_start + pos + rel;
                        while k + 1 < ke && (table.offs[k + 1] as usize) <= hit {
                            k += 1;
                        }
                        let id = table.ids[k];
                        let off = table.offs[k] as usize;
                        let end = off + idx.name(id).len();
                        if hit + needle_len <= end && anchor(hit, off, end) {
                            if accept(id) {
                                out.push(id);
                            }
                            // One hit per entry is enough: resume at its end.
                            pos = end - pool_start;
                        } else if hit >= end {
                            // Stale gap between entries: jump to the next entry.
                            let next = if k + 1 < table.len() {
                                (table.offs[k + 1] as usize).min(pool_end)
                            } else {
                                pool_end
                            };
                            pos = next.max(hit + 1) - pool_start;
                        } else {
                            pos = hit + 1 - pool_start;
                        }
                    }
                };

            match driver {
                Driver::Sub {
                    finder, needle_len, ..
                } => {
                    sweep(*needle_len, &mut |h| finder.find(h), &mut |_, _, _| true);
                }
                Driver::Prefix { bytes, .. } => {
                    let finder = memchr::memmem::Finder::new(bytes);
                    sweep(bytes.len(), &mut |h| finder.find(h), &mut |hit, off, _| {
                        hit == off
                    });
                }
                Driver::Suffixes {
                    suffixes,
                    files_only,
                    ..
                } => {
                    // Anchored tails defeat memmem's rare-byte prefilter
                    // ('.' occurs in almost every name), so a sequential
                    // table-order tail compare wins here.
                    for k in ks..ke {
                        let id = table.ids[k];
                        let off = table.offs[k] as usize;
                        let name = &pool[off..off + idx.name(id).len()];
                        if suffixes.iter().any(|s| name.ends_with(s))
                            && (!*files_only || !idx.is_dir(id))
                            && accept(id)
                        {
                            out.push(id);
                        }
                    }
                }
                _ => unreachable!(),
            }
            out
        })
        .collect();
    chunks.concat()
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
        cached_memo = idx.cached_derived(|| DirPaths::build(idx));
        &cached_memo
    } else {
        empty_memo = DirPaths {
            lower: Vec::new(),
            orig: Vec::new(),
        };
        &empty_memo
    };
    let needs_table = q
        .groups
        .iter()
        .any(|g| !matches!(g.driver, Driver::FullScan | Driver::MatchAll));
    let table: Option<std::sync::Arc<OffsetTable>> = if needs_table {
        Some(idx.cached_derived(|| OffsetTable::build(idx)))
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
    }
}
