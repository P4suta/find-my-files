//! Micro-benchmarks for the query path over a synthetic 1M-entry index whose name distribution is calibrated to the real-C: measurement (ADR-0013).
//! Run via `just bench-micro`; compare before/after every kernel change.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use fmf_runtime::engine::{Engine, EngineConfig};
use fmf_runtime::index::testutil::merge_new_into_permutations;
use fmf_runtime::index::{Frn, RawEntry, SortKey, VolumeIndex, VolumeIndexBuilder};
use fmf_runtime::query::{self, CaseMode, QueryOptions, UtcResolver};

/// Deterministic xorshift so every run sees identical data.
struct Rng(u64);
impl Rng {
    const fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const DIRS: usize = 1_000;
const FILES_PER_DIR: usize = 1_000;

/// Asserted at the end of `build_synthetic`: silent distribution drift off the measured real-C: ratios would invalidate every micro-baseline (ADR-0013).
/// Re-calibrate via `fmf stats --name-stats`.
const FOLD_IDENTICAL_RANGE: std::ops::RangeInclusive<f64> = 0.70..=0.76;
const UNIQUE_RANGE: std::ops::RangeInclusive<f64> = 0.50..=0.56;
const MEAN_LEN_RANGE: std::ops::RangeInclusive<f64> = 28.0..=32.0;

fn build_synthetic_unfinished() -> VolumeIndexBuilder {
    use std::collections::HashSet;
    use std::hash::{DefaultHasher, Hash, Hasher};

    let mut rng = Rng(0x5EED_F00D);
    let words = [
        "report",
        "window",
        "backup",
        "config",
        "module",
        "shader",
        "vendor",
        "sample",
        "update",
        "library",
        "プロジェクト",
        "資料",
    ];
    let exts = ["txt", "dll", "rs", "png", "log", "json", "exe", "dat"];

    let cap_first = |w: &str| {
        let mut cs = w.chars();
        match cs.next() {
            Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
            None => String::new(),
        }
    };
    let fresh = |rng: &mut Rng| -> String {
        let r = rng.next();
        let r2 = rng.next();
        let w1 = words[(r >> 4) as usize % words.len()];
        let w2 = words[(r >> 12) as usize % words.len()];
        let ext = exts[(r >> 20) as usize % exts.len()];
        let tag = (r >> 24) & 0xFF_FFFF;
        let upper = r2 % 100 < 27;
        let style = |w: &str| if upper { cap_first(w) } else { w.to_string() };
        if (r2 >> 8).is_multiple_of(2) {
            let w3 = words[(r2 >> 16) as usize % words.len()];
            format!("{}_{}_{}_{tag:06x}.{ext}", style(w1), style(w2), style(w3))
        } else {
            format!("{}_{}_{tag:06x}.{ext}", style(w1), style(w2))
        }
    };

    let (mut total, mut byte_sum, mut cased) = (0u64, 0u64, 0u64);
    let mut uniq: HashSet<u64> = HashSet::new();
    let mut tally = |name: &str| {
        total += 1;
        byte_sum += name.len() as u64;
        if name.bytes().any(|b| b.is_ascii_uppercase()) {
            cased += 1;
        }
        let mut h = DefaultHasher::new();
        name.hash(&mut h);
        uniq.insert(h.finish());
    };
    let mut pool: Vec<String> = Vec::new();

    let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
    let mut record = 100u64;
    for d in 0..DIRS {
        let dir_record = record;
        record += 1;
        let dir_name = if d == 0 {
            "windows".to_string()
        } else {
            format!("dir_{d:04}_{}", words[(rng.next() % 8) as usize])
        };
        tally(&dir_name);
        let units: Vec<u16> = dir_name.encode_utf16().collect();
        b.push(RawEntry {
            parent_frn: Frn(5),
            frn: Frn((1 << 48) | dir_record),
            name_utf16: &units,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        for f in 0..FILES_PER_DIR {
            let name = if d == 1 {
                let r = rng.next();
                format!(
                    "file_0001_{f:04}_{}_{:04x}.{}",
                    words[(r % 10) as usize],
                    r >> 48,
                    exts[((r >> 8) % 8) as usize]
                )
            } else if !pool.is_empty() && rng.next() % 100 < 47 {
                pool[(rng.next() as usize) % pool.len()].clone()
            } else {
                let n = fresh(&mut rng);
                pool.push(n.clone());
                n
            };
            tally(&name);
            let r = rng.next();
            let size = if d == 2 && f < 4 {
                (5 + f as u64) << 30
            } else {
                r % (1 << 30)
            };
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                parent_frn: Frn(dir_record),
                frn: Frn((1 << 48) | record),
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size,
                mtime: (r % (1 << 40)) as i64,
            });
            record += 1;
        }
    }
    let fold_identical = 1.0 - cased as f64 / total as f64;
    let unique = uniq.len() as f64 / total as f64;
    let mean = byte_sum as f64 / total as f64;
    assert!(
        FOLD_IDENTICAL_RANGE.contains(&fold_identical),
        "synthetic fold-identical ratio drifted off real-C:: {fold_identical:.3}"
    );
    assert!(
        UNIQUE_RANGE.contains(&unique),
        "synthetic unique-name ratio drifted off real-C:: {unique:.3}"
    );
    assert!(
        MEAN_LEN_RANGE.contains(&mean),
        "synthetic mean name length drifted off real-C:: {mean:.1}"
    );
    b
}

/// The synthetic 1M-entry index, finalized and queryable.
fn build_synthetic() -> VolumeIndex {
    build_synthetic_unfinished().finish()
}

fn bench_queries(c: &mut Criterion) {
    let idx = build_synthetic();
    let opt = QueryOptions::default();

    let cases: &[(&str, &str)] = &[
        ("match_all", ""),
        ("one_char", "e"),
        ("common", "win"),
        ("upper_smart", "Win"),
        ("rare", "qzx9"),
        ("canonical_nfd_miss", "résumé"),
        ("ext", "ext:dll"),
        ("wildcard_suffix", "*.rs"),
        ("composite_path", "size:>100mb path:windows"),
        ("negation", "report !backup"),
        ("regex", "regex:win.*\\.dll"),
        ("regex_suffix", "regex:\\.dll$"),
        ("regex_scan", "regex:[0-9]{4}x"),
    ];

    let mut g = c.benchmark_group("query");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(4));
    for (label, text) in cases {
        let ast = query::parse(text).expect("valid benchmark fixture");
        let compiled =
            query::compile(&ast, opt.case, &UtcResolver).expect("valid benchmark fixture");
        g.bench_function(*label, |b| {
            b.iter(|| {
                let (r, _) = query::search(&idx, &compiled, &opt);
                std::hint::black_box(r.ids.len())
            });
        });
    }
    let sensitive = QueryOptions {
        case: CaseMode::Sensitive,
        ..QueryOptions::default()
    };
    let ast = query::parse("Report").expect("valid benchmark fixture");
    let q = query::compile(&ast, sensitive.case, &UtcResolver).expect("valid benchmark fixture");
    g.bench_function("case_sensitive", |b| {
        b.iter(|| {
            let (r, _) = query::search(&idx, &q, &sensitive);
            std::hint::black_box(r.ids.len())
        });
    });
    g.finish();

    c.bench_function("parse_compile", |b| {
        b.iter(|| {
            let ast =
                query::parse("report ext:dll size:>1mb !backup").expect("valid benchmark fixture");
            std::hint::black_box(
                query::compile(&ast, opt.case, &UtcResolver).expect("valid benchmark fixture"),
            )
        });
    });
}

/// One keystroke through the engine's incremental query cache: `setup` seeds the per-volume cache, `routine` runs the next keystroke.
/// The refine/cold pairs share the routine query, so the delta is purely the cache path (engine/search.rs + query/subsume.rs + `exec::refine`).
fn bench_typing(c: &mut Criterion) {
    let engine = Engine::new(EngineConfig {
        index_dir: std::env::temp_dir().join(format!("fmf-bench-typing-{}", std::process::id())),
    })
    .expect("bench engine");
    engine.insert_ready_volume("C:", build_synthetic());
    let opt = QueryOptions::default();

    engine.query("win", &opt).expect("valid benchmark fixture");
    let (_, t) = engine.query("wind", &opt).expect("valid benchmark fixture");
    assert_eq!(t.cache, "refine", "typing bench setup is broken");
    let (_, t) = engine.query("qzx9", &opt).expect("valid benchmark fixture");
    assert_eq!(t.cache, "miss");

    let mut g = c.benchmark_group("typing");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(4));
    let steps: &[(&str, &str, &str)] = &[
        ("refine_wind", "win", "wind"),
        ("cold_wind", "qzx9", "wind"),
        ("refine_e_from_matchall", "", "e"),
        ("cold_e", "qzx9", "e"),
        ("refine_add_filter", "report", "report ext:dll"),
        ("cold_add_filter", "qzx9", "report ext:dll"),
        ("refine_narrow", "file_0001_", "file_0001_05"),
        ("cold_narrow", "qzx9", "file_0001_05"),
    ];
    for (label, seed, keystroke) in steps {
        g.bench_function(*label, |b| {
            b.iter_batched(
                || {
                    engine.query(seed, &opt).expect("valid benchmark fixture");
                },
                |()| {
                    let (r, t) = engine
                        .query(keystroke, &opt)
                        .expect("valid benchmark fixture");
                    std::hint::black_box((r.len(), t.cache.len()))
                },
                BatchSize::PerIteration,
            );
        });
    }
    g.finish();
}

/// First query after a USN batch: the content generation moved, so the derived caches (offset table, dir paths) must be re-established.
/// Setup bumps the generation without growing the index — the measured delta between full rebuild and incremental extend is then size-stable.
fn bench_post_usn(c: &mut Criterion) {
    let idx = std::cell::RefCell::new(build_synthetic());
    let opt = QueryOptions::default();
    let ast = query::parse("win").expect("valid benchmark fixture");
    let compiled = query::compile(&ast, opt.case, &UtcResolver).expect("valid benchmark fixture");

    let mut group = c.benchmark_group("post_usn");
    group.sample_size(20);
    group.measurement_time(std::time::Duration::from_secs(4));
    group.bench_function("first_query_win", |b| {
        b.iter_batched(
            || {
                let mut i = idx.borrow_mut();
                let len = i.len() as u32;
                merge_new_into_permutations(&mut i, len);
            },
            |()| {
                let i = idx.borrow();
                let (r, m) = query::search(&i, &compiled, &opt);
                std::hint::black_box((r.ids.len(), m.memo_us))
            },
            BatchSize::PerIteration,
        );
    });
    let path_ast = query::parse("path:windows report").expect("valid benchmark fixture");
    let path_q =
        query::compile(&path_ast, opt.case, &UtcResolver).expect("valid benchmark fixture");
    group.bench_function("first_query_path", |b| {
        b.iter_batched(
            || {
                let mut i = idx.borrow_mut();
                let len = i.len() as u32;
                merge_new_into_permutations(&mut i, len);
            },
            |()| {
                let i = idx.borrow();
                let (r, m) = query::search(&i, &path_q, &opt);
                std::hint::black_box((r.ids.len(), m.memo_us))
            },
            BatchSize::PerIteration,
        );
    });
    let size_opt = QueryOptions {
        sort: SortKey::Size,
        ..QueryOptions::default()
    };
    let empty_ast = query::parse("").expect("valid benchmark fixture");
    let empty_q =
        query::compile(&empty_ast, size_opt.case, &UtcResolver).expect("valid benchmark fixture");
    group.bench_function("first_query_sorted_size", |b| {
        b.iter_batched(
            || {
                let mut i = idx.borrow_mut();
                let len = i.len() as u32;
                merge_new_into_permutations(&mut i, len);
            },
            |()| {
                let i = idx.borrow();
                let (r, m) = query::search(&i, &empty_q, &size_opt);
                std::hint::black_box((r.ids.len(), m.memo_us))
            },
            BatchSize::PerIteration,
        );
    });
    let batch_idx = std::cell::RefCell::new(build_synthetic());
    let names: Vec<Vec<u16>> = (0..16)
        .map(|i| format!("usn_new_{i:02}.tmp").encode_utf16().collect())
        .collect();
    let iter_no = std::cell::Cell::new(0u64);
    group.bench_function("apply_batch_1k", |b| {
        b.iter_batched(
            || {
                let n = iter_no.get();
                iter_no.set(n + 1);
                n
            },
            |n| {
                let mut i = batch_idx.borrow_mut();
                let first_new = i.len() as u32;
                for k in 0..700u64 {
                    let rec = 50_000_000 + n * 700 + k;
                    i.upsert_synthetic(&RawEntry {
                        parent_frn: Frn(100),
                        frn: Frn((1 << 48) | rec),
                        name_utf16: &names[(k % 16) as usize],
                        is_dir: false,
                        is_reparse: false,
                        is_hidden: false,
                        is_system: false,
                        size: k,
                        mtime: k as i64,
                    });
                }
                for k in 0..300u64 {
                    i.delete(100 + ((n * 300 + k) * 7919) % 900_000);
                }
                merge_new_into_permutations(&mut i, first_new);
                std::hint::black_box(i.len());
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

/// Snapshot restore (deserialize + checksum + `frn_map` rebuild) on the synthetic 1M index — the unelevated proxy for the restore→ready ≤2s gate's CPU-bound share (page-cache warm by design).
fn bench_snapshot(c: &mut Criterion) {
    let idx = build_synthetic();
    let path = std::env::temp_dir().join(format!("fmf-bench-snap-{}.fmfidx", std::process::id()));
    idx.save_to(&path, 1, 1).expect("valid benchmark fixture");

    let mut g = c.benchmark_group("snapshot");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(6));
    g.bench_function("load_1m", |b| {
        b.iter(|| {
            let (loaded, _, _) = VolumeIndex::load_from(&path).expect("valid benchmark fixture");
            std::hint::black_box(loaded.len())
        });
    });
    g.finish();
    let _ = std::fs::remove_file(&path);
}

/// Initial-build finalize on the synthetic 1M index: parent resolution,
/// dictionary dedup, and the name-permutation sort (ADR-0033 "1A" build-rank — distinct names ranked once, entries sorted on a packed integer key).
/// Informational only: synthetic criterion timings carry ±12-23% layout noise (ADR-0013), so the real arbiter of the build-rank win is the real-volume scan time in `just bench-check C:`, judged back-to-back in one session.
fn bench_build(c: &mut Criterion) {
    let mut g = c.benchmark_group("build");
    g.sample_size(10);
    g.measurement_time(std::time::Duration::from_secs(8));
    g.bench_function("finish_1m", |b| {
        b.iter_batched(
            build_synthetic_unfinished,
            |builder| std::hint::black_box(builder.finish().len()),
            BatchSize::PerIteration,
        );
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_queries,
    bench_typing,
    bench_post_usn,
    bench_snapshot,
    bench_build
);
criterion_main!(benches);
