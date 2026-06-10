//! Micro-benchmarks for the query path over a synthetic 1M-entry index
//! whose name-length distribution (~30 UTF-16 units) matches the real-C:
//! measurement. Run via `just bench-micro`; compare before/after every
//! kernel change.

use criterion::{Criterion, criterion_group, criterion_main};
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

criterion_group!(benches, bench_queries);
criterion_main!(benches);
