//! Micro-benchmarks for the query path over a synthetic 1M-entry index
//! whose name-length distribution (~30 UTF-16 units) matches the real-C:
//! measurement. Run via `just bench-micro`; compare before/after every
//! kernel change.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use fmf_core::engine::{Engine, EngineConfig};
use fmf_core::index::{RawEntry, VolumeIndex, VolumeIndexBuilder};
use fmf_core::query::{self, QueryOptions, UtcResolver};

/// Deterministic xorshift so every run sees identical data.
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

const DIRS: usize = 1_000;
const FILES_PER_DIR: usize = 1_000;

fn build_synthetic() -> VolumeIndex {
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
        "プロジェクト",
        "資料",
    ];
    let exts = ["txt", "dll", "rs", "png", "log", "json", "exe", "dat"];

    let mut b = VolumeIndexBuilder::new("C:", 5);
    let mut record = 100u64;
    for d in 0..DIRS {
        let dir_record = record;
        record += 1;
        // One recognizable branch for path: benchmarks.
        let dir_name = if d == 0 {
            "windows".to_string()
        } else {
            format!("dir_{d:04}_{}", words[(rng.next() % 8) as usize])
        };
        let units: Vec<u16> = dir_name.encode_utf16().collect();
        b.push(RawEntry {
            record: dir_record,
            parent_record: 5,
            frn: (1 << 48) | dir_record,
            name_utf16: &units,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        for f in 0..FILES_PER_DIR {
            let r = rng.next();
            let name = format!(
                "file_{d:04}_{f:04}_{}_{:04x}.{}",
                words[(r % 10) as usize],
                r >> 48,
                exts[((r >> 8) % 8) as usize]
            );
            let units: Vec<u16> = name.encode_utf16().collect();
            b.push(RawEntry {
                record,
                parent_record: dir_record,
                frn: (1 << 48) | record,
                name_utf16: &units,
                is_dir: false,
                is_reparse: false,
                is_hidden: false,
                is_system: false,
                size: r % (1 << 30),
                mtime: (r % (1 << 40)) as i64,
            });
            record += 1;
        }
    }
    b.finish()
}

fn bench_queries(c: &mut Criterion) {
    let idx = build_synthetic();
    let opt = QueryOptions::default();

    let cases: &[(&str, &str)] = &[
        ("match_all", ""),
        ("one_char", "e"),
        ("common", "win"),
        ("rare", "qzx9"),
        ("ext", "ext:dll"),
        ("wildcard_suffix", "*.rs"),
        ("composite_path", "size:>100mb path:windows"),
        ("negation", "report !backup"),
    ];

    let mut g = c.benchmark_group("query");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(4));
    for (label, text) in cases {
        let ast = query::parse(text).unwrap();
        let compiled = query::compile(&ast, opt.case, &UtcResolver).unwrap();
        g.bench_function(*label, |b| {
            b.iter(|| {
                let (r, _) = query::search(&idx, &compiled, &opt);
                std::hint::black_box(r.ids.len())
            })
        });
    }
    g.finish();

    // Parse+compile overhead on its own.
    c.bench_function("parse_compile", |b| {
        b.iter(|| {
            let ast = query::parse("report ext:dll size:>1mb !backup").unwrap();
            std::hint::black_box(query::compile(&ast, opt.case, &UtcResolver).unwrap())
        })
    });
}

/// One keystroke through the engine's incremental query cache: `setup`
/// seeds the per-volume cache, `routine` runs the next keystroke. The
/// refine/cold pairs share the routine query, so the delta is purely the
/// cache path (engine/search.rs + query/subsume.rs + exec::refine).
fn bench_typing(c: &mut Criterion) {
    let engine = Engine::new(EngineConfig {
        index_dir: std::env::temp_dir(),
    });
    engine.insert_ready_volume("C:", build_synthetic());
    let opt = QueryOptions::default();

    // Sanity: the pairs must actually take the intended paths.
    engine.query("win", &opt).unwrap();
    let (_, t) = engine.query("wind", &opt).unwrap();
    assert_eq!(t.cache, "refine", "typing bench setup is broken");
    let (_, t) = engine.query("qzx9", &opt).unwrap();
    assert_eq!(t.cache, "miss");

    let mut g = c.benchmark_group("typing");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(4));
    // (label, cache-seeding query, measured keystroke). The refine win
    // scales with how selective the seed already was: `wind` keeps ~10% of
    // the index matching (worst realistic case), `file_0001_` starts from
    // ~1k hits (the common deep-typing case).
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
                    engine.query(seed, &opt).unwrap();
                },
                |()| {
                    let (r, t) = engine.query(keystroke, &opt).unwrap();
                    std::hint::black_box((r.len(), t.cache.len()))
                },
                BatchSize::PerIteration,
            )
        });
    }
    g.finish();
}

/// First query after a USN batch: the content generation moved, so the
/// derived caches (offset table, dir paths) must be re-established. Setup
/// bumps the generation without growing the index — the measured delta
/// between full rebuild and incremental extend is then size-stable.
fn bench_post_usn(c: &mut Criterion) {
    let idx = std::cell::RefCell::new(build_synthetic());
    let opt = QueryOptions::default();
    let ast = query::parse("win").unwrap();
    let compiled = query::compile(&ast, opt.case, &UtcResolver).unwrap();

    let mut g = c.benchmark_group("post_usn");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(4));
    g.bench_function("first_query_win", |b| {
        b.iter_batched(
            || {
                let mut i = idx.borrow_mut();
                let len = i.len() as u32;
                i.merge_new_into_permutations(len); // empty batch: generation++
            },
            |()| {
                let i = idx.borrow();
                let (r, m) = query::search(&i, &compiled, &opt);
                std::hint::black_box((r.ids.len(), m.memo_us))
            },
            BatchSize::PerIteration,
        )
    });
    g.finish();
}

/// Snapshot restore (deserialize + checksum + frn_map rebuild) on the
/// synthetic 1M index — the unelevated proxy for the restore→ready ≤2s
/// gate's CPU-bound share (page-cache warm by design).
fn bench_snapshot(c: &mut Criterion) {
    let idx = build_synthetic();
    let path = std::env::temp_dir().join(format!("fmf-bench-snap-{}.fmfidx", std::process::id()));
    idx.save_to(&path, 1, 1).unwrap();

    let mut g = c.benchmark_group("snapshot");
    g.sample_size(20);
    g.measurement_time(std::time::Duration::from_secs(6));
    g.bench_function("load_1m", |b| {
        b.iter(|| {
            let (loaded, _, _) = VolumeIndex::load_from(&path).unwrap();
            std::hint::black_box(loaded.len())
        })
    });
    g.finish();
    let _ = std::fs::remove_file(&path);
}

criterion_group!(
    benches,
    bench_queries,
    bench_typing,
    bench_post_usn,
    bench_snapshot
);
criterion_main!(benches);
