//! fmf — developer CLI for the find-my-files engine.

use std::io::{BufRead, Write};
use std::time::Instant;

use clap::{Parser, Subcommand};
use fmf_core::index::{SortKey, VolumeIndex};
use fmf_core::query::{self, CaseMode, QueryOptions};

#[derive(Parser)]
#[command(name = "fmf", about = "find-my-files engine developer CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Spike S0: scan a volume's $MFT and print raw measurements.
    Spike { drive: String },
    /// Build the index for a volume, print stats, then run an interactive
    /// query REPL (requires an elevated terminal).
    Index {
        drive: String,
        /// Print stats and exit without the REPL.
        #[arg(long)]
        stats: bool,
        /// Include hidden/system entries (excluded by default, like the app).
        #[arg(long)]
        include_hidden_system: bool,
    },
    /// Index a volume and run the fixed benchmark query set.
    Bench {
        drive: String,
        /// Write the full report as JSON.
        #[arg(long)]
        json: Option<std::path::PathBuf>,
        /// Compare against a previous --json report; exit 1 when p50 or p99
        /// regress by more than 20%.
        #[arg(long)]
        baseline: Option<std::path::PathBuf>,
    },
    /// Index a volume and dump per-column memory accounting as JSON.
    Stats { drive: String },
    /// Print versions, log locations and the in-process diagnostics ring.
    Diag,
    /// Index a volume, then tail its USN journal and apply changes live,
    /// printing one line per applied batch (Ctrl+C to stop).
    Watch { drive: String },
    /// Gate criterion micro-bench results: scan change reports written by
    /// `cargo bench -- --baseline <name>` and exit 1 past the threshold
    /// (criterion itself never sets an exit code on regressions).
    CriterionGate {
        /// Criterion output directory.
        #[arg(long, default_value = "target/criterion")]
        dir: std::path::PathBuf,
        /// Relative median regression threshold (0.10 = +10%).
        #[arg(long, default_value_t = 0.10)]
        threshold: f64,
    },
}

#[cfg(windows)]
fn date_resolver() -> impl query::DateResolver {
    query::WindowsLocalResolver
}
#[cfg(not(windows))]
fn date_resolver() -> impl query::DateResolver {
    query::UtcResolver
}

fn main() {
    // Same pipeline as the DLL: stderr log + diag ring + panic capture.
    fmf_core::diag::init_logging(None, "info");
    fmf_core::diag::install_panic_hook();

    let cli = Cli::parse();
    let result = match cli.command {
        Command::Spike { drive } => spike(&drive),
        Command::Index {
            drive,
            stats,
            include_hidden_system,
        } => index(&drive, stats, include_hidden_system),
        Command::Bench {
            drive,
            json,
            baseline,
        } => bench(&drive, json.as_deref(), baseline.as_deref()),
        Command::Stats { drive } => stats(&drive),
        Command::Diag => diag(),
        Command::Watch { drive } => watch(&drive),
        Command::CriterionGate { dir, threshold } => criterion_gate(&dir, threshold),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        let mut source = e.source();
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
        std::process::exit(1);
    }
}

fn spike(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
    let s = fmf_core::mft::spike_scan(drive)?;
    let named = s.files + s.dirs;
    println!("volume               : {}", s.volume);
    println!("open volume          : {} ms", s.elapsed_volume_open_ms);
    println!(
        "load $MFT            : {} ms  ({:.1} MiB)",
        s.elapsed_mft_load_ms,
        s.mft_bytes as f64 / (1024.0 * 1024.0)
    );
    println!("iterate records      : {} ms", s.elapsed_iterate_ms);
    println!("total records        : {}", s.total_records);
    println!("files / dirs         : {} / {}", s.files, s.dirs);
    println!("reparse points       : {}", s.reparse_points);
    println!("no-name base records : {}", s.no_name_in_base_record);
    println!(
        "avg/max name length  : {:.1} / {} UTF-16 units",
        s.avg_name_utf16_units(),
        s.max_name_utf16_units
    );
    println!(
        "FRN sequence nonzero : {} / {}",
        s.frn_sequence_nonzero, named
    );
    println!(
        "peak working set     : {:.1} MiB",
        s.peak_working_set_bytes as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

fn build_index(drive: &str) -> Result<VolumeIndex, Box<dyn std::error::Error>> {
    let (idx, s) = fmf_core::mft::scan_volume(drive)?;
    let per_entry = if !idx.is_empty() {
        s.peak_working_set_bytes / idx.len() as u64
    } else {
        0
    };
    eprintln!(
        "indexed {} entries ({} files, {} dirs, {} skipped) in {} ms ($MFT read {} ms, parse {} ms, build {} ms, sort {} ms — read/parse overlap)",
        idx.len(),
        s.files,
        s.dirs,
        s.skipped_no_name,
        s.elapsed_total_ms,
        s.elapsed_mft_load_ms,
        s.elapsed_parse_ms,
        s.elapsed_build_ms,
        s.elapsed_sort_ms
    );
    eprintln!(
        "peak working set {:.1} MiB (≈{} B/entry incl. $MFT buffer)",
        s.peak_working_set_bytes as f64 / (1024.0 * 1024.0),
        per_entry
    );
    Ok(idx)
}

fn run_query(
    idx: &VolumeIndex,
    input: &str,
    opt: QueryOptions,
) -> Result<(query::SearchResult, query::SearchMetrics), Box<dyn std::error::Error>> {
    let ast = query::parse(input)?;
    let q = query::compile(&ast, opt.case, &date_resolver())?;
    Ok(query::search(idx, &q, &opt))
}

fn index(
    drive: &str,
    stats_only: bool,
    include_hidden_system: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive)?;
    if stats_only {
        return Ok(());
    }

    let opt = QueryOptions {
        sort: SortKey::Name,
        desc: false,
        case: CaseMode::Smart,
        include_hidden_system,
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    eprintln!("query REPL — empty line to quit");
    loop {
        eprint!("> ");
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let input = line.trim();
        if input.is_empty() {
            break;
        }
        let t = Instant::now();
        match run_query(&idx, input, opt) {
            Ok((r, m)) => {
                let elapsed = t.elapsed();
                let mut path = Vec::new();
                for &id in r.ids.iter().take(20) {
                    path.clear();
                    idx.append_path(id, &mut path);
                    out.write_all(&path)?;
                    out.write_all(b"\n")?;
                }
                println!(
                    "-- {} hits in {:?} (memo {}µs, scan {}µs, materialize {}µs, {} scanned, {} excluded)",
                    r.ids.len(),
                    elapsed,
                    m.memo_us,
                    m.scan_us,
                    m.materialize_us,
                    m.entries_scanned,
                    m.excluded_skipped
                );
            }
            Err(e) => println!("query error: {e}"),
        }
    }
    Ok(())
}

/// Scan, then tail the journal. The checkpoint is taken *before* the scan so
/// changes made during the scan are replayed, not lost (ARCHITECTURE.md).
fn watch(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
    use fmf_core::usn::{ReadOutcome, UsnJournal, VolumeStatFetcher, apply_batch};

    let mut journal = UsnJournal::open(drive, None)?;
    let mut idx = build_index(drive)?;
    let fetch = VolumeStatFetcher::open(drive)?;
    eprintln!(
        "watching {drive} (journal id {:#x}, from usn {}) — Ctrl+C to stop",
        journal.journal_id, journal.next_usn
    );
    let mut buf = Vec::new();
    loop {
        match journal.read_blocking(&mut buf)? {
            ReadOutcome::Records {
                records: rs,
                truncated,
            } => {
                if truncated {
                    eprintln!("warning: USN batch had malformed tail bytes");
                }
                if rs.is_empty() {
                    continue;
                }
                let s = apply_batch(&mut idx, &rs, &fetch);
                eprintln!(
                    "{} records → {} upserted, {} deleted, {} stat, {} ignored (live {})",
                    rs.len(),
                    s.created_or_renamed,
                    s.deleted,
                    s.stat_updated,
                    s.ignored,
                    idx.live_len()
                );
            }
            ReadOutcome::Gone(g) => {
                eprintln!("journal unavailable ({g:?}) — full rescan required, exiting");
                break;
            }
        }
    }
    Ok(())
}

const BENCH_QUERIES: &[&str] = &[
    "",                         // match-all (UI shows this on launch)
    "e",                        // 1 char, huge hit count
    "a",                        // 1 char, huge hit count
    "win",                      // common 3-char substring
    "qzx",                      // rare substring
    "ext:dll",                  // extension filter
    "size:>100mb path:windows", // composite
    "*.rs",                     // wildcard
];

#[derive(serde::Serialize, serde::Deserialize)]
struct QueryBench {
    query: String,
    hits: u64,
    p50_us: u64,
    p99_us: u64,
    max_us: u64,
    p50_memo_us: u64,
    p50_scan_us: u64,
    p50_materialize_us: u64,
    /// First iteration of the run — the only one that pays cold derived-cache
    /// builds (memo/offset-table). Single sample: recorded, never gated.
    #[serde(default)]
    cold_us: u64,
}

/// Snapshot save/restore timings (page-cache warm: the reproducible
/// CPU-bound part of the ≤2s restore gate; cold I/O is not benchable
/// without admin cache-purge APIs and is too noisy anyway).
#[derive(serde::Serialize, serde::Deserialize)]
struct RestoreBench {
    file_bytes: u64,
    entries: u64,
    save_ms: u64,
    p50_ms: u64,
    min_ms: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BenchReport {
    volume: String,
    entries: u64,
    peak_working_set_bytes: u64,
    queries: Vec<QueryBench>,
    /// Absent in baselines recorded before the restore scenario existed.
    #[serde(default)]
    restore: Option<RestoreBench>,
}

fn median(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}

fn bench(
    drive: &str,
    json: Option<&std::path::Path>,
    baseline: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive)?;
    let opt = QueryOptions::default();

    let mut report = BenchReport {
        volume: drive.to_string(),
        entries: idx.len() as u64,
        peak_working_set_bytes: 0,
        queries: Vec::new(),
        restore: None,
    };

    println!(
        "{:<28} {:>10} {:>9} {:>9} {:>9} {:>9} | {:>8} {:>8} {:>8}",
        "query", "hits", "p50_us", "p99_us", "max_us", "cold_us", "memo", "scan", "mat"
    );
    for q in BENCH_QUERIES {
        const RUNS: usize = 50;
        let mut totals = Vec::with_capacity(RUNS);
        let (mut memos, mut scans, mut mats) = (Vec::new(), Vec::new(), Vec::new());
        let mut hits = 0u64;
        for _ in 0..RUNS {
            let t = Instant::now();
            let (r, m) = run_query(&idx, q, opt)?;
            totals.push(t.elapsed().as_micros() as u64);
            memos.push(m.memo_us);
            scans.push(m.scan_us);
            mats.push(m.materialize_us);
            hits = r.ids.len() as u64;
        }
        let cold_us = totals[0];
        totals.sort_unstable();
        let qb = QueryBench {
            query: q.to_string(),
            hits,
            p50_us: totals[RUNS / 2],
            p99_us: totals[RUNS * 99 / 100],
            max_us: totals[RUNS - 1],
            p50_memo_us: median(memos),
            p50_scan_us: median(scans),
            p50_materialize_us: median(mats),
            cold_us,
        };
        println!(
            "{:<28} {:>10} {:>9} {:>9} {:>9} {:>9} | {:>8} {:>8} {:>8}",
            qb.query,
            qb.hits,
            qb.p50_us,
            qb.p99_us,
            qb.max_us,
            qb.cold_us,
            qb.p50_memo_us,
            qb.p50_scan_us,
            qb.p50_materialize_us
        );
        report.queries.push(qb);
    }
    report.peak_working_set_bytes = fmf_core::mft::peak_working_set();
    println!(
        "peak working set {:.1} MiB",
        report.peak_working_set_bytes as f64 / (1024.0 * 1024.0)
    );

    report.restore = Some(bench_restore(&idx)?);
    if let Some(r) = &report.restore {
        println!(
            "snapshot save {} ms; restore p50 {} ms / min {} ms ({:.1} MiB, {} entries)",
            r.save_ms,
            r.p50_ms,
            r.min_ms,
            r.file_bytes as f64 / (1024.0 * 1024.0),
            r.entries
        );
    }

    if let Some(path) = json {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        eprintln!("report written to {}", path.display());
    }

    if let Some(path) = baseline {
        let old: BenchReport = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        // A live volume drifts; past ±10% the 20% gate compares apples to
        // oranges — tell the human to re-record rather than chase ghosts.
        if report.entries.abs_diff(old.entries) > old.entries / 10 {
            eprintln!(
                "WARNING entries drifted {}→{} (>10%) since the baseline was recorded — \
                 regression verdicts are unreliable; consider `just bench-baseline`",
                old.entries, report.entries
            );
        }
        let mut regressed = false;
        // Flag >20% regressions above a noise floor (`floor`, in the value's
        // own unit: 200µs for queries, 50ms for restore).
        let gate = |new: u64, old: u64, floor: u64| new > old.max(floor) + old.max(floor) / 5;
        for qb in &report.queries {
            let Some(prev) = old.queries.iter().find(|p| p.query == qb.query) else {
                continue;
            };
            if gate(qb.p50_us, prev.p50_us, 200) || gate(qb.p99_us, prev.p99_us, 200) {
                eprintln!(
                    "REGRESSION {:<24} p50 {}→{}µs p99 {}→{}µs",
                    qb.query, prev.p50_us, qb.p50_us, prev.p99_us, qb.p99_us
                );
                regressed = true;
            }
        }
        if let (Some(new), Some(prev)) = (&report.restore, &old.restore)
            && gate(new.p50_ms, prev.p50_ms, 50)
        {
            eprintln!(
                "REGRESSION snapshot restore p50 {}→{}ms",
                prev.p50_ms, new.p50_ms
            );
            regressed = true;
        }
        if regressed {
            return Err("benchmark regression vs baseline".into());
        }
        eprintln!("no regression vs {}", path.display());
    }
    Ok(())
}

/// Save the freshly built index to a temp snapshot and measure restores.
/// Page-cache-warm by design: reproducible CPU-bound numbers for the
/// restore→ready gate's deserialization + frn_map rebuild share.
fn bench_restore(idx: &VolumeIndex) -> Result<RestoreBench, Box<dyn std::error::Error>> {
    const RUNS: usize = 10;
    let temp = std::env::temp_dir().join(format!("fmf-bench-{}.fmfidx", std::process::id()));
    let t = Instant::now();
    idx.save_to(&temp, 0, 0)?;
    let save_ms = t.elapsed().as_millis() as u64;
    let file_bytes = std::fs::metadata(&temp)?.len();

    let mut runs = Vec::with_capacity(RUNS);
    let mut entries = 0u64;
    for _ in 0..RUNS {
        let t = Instant::now();
        let (loaded, _, _) = VolumeIndex::load_from(&temp)?;
        runs.push(t.elapsed().as_millis() as u64);
        entries = loaded.len() as u64;
    }
    let _ = std::fs::remove_file(&temp);
    runs.sort_unstable();
    Ok(RestoreBench {
        file_bytes,
        entries,
        save_ms,
        p50_ms: runs[RUNS / 2],
        min_ms: runs[0],
    })
}

/// Collect `<bench>/change/estimates.json` paths under criterion's output dir.
fn collect_change_reports(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_change_reports(&p, out);
        } else if p.file_name().is_some_and(|f| f == "estimates.json")
            && p.parent()
                .and_then(|d| d.file_name())
                .is_some_and(|d| d == "change")
        {
            out.push(p);
        }
    }
}

fn criterion_gate(dir: &std::path::Path, threshold: f64) -> Result<(), Box<dyn std::error::Error>> {
    let mut reports = Vec::new();
    collect_change_reports(dir, &mut reports);
    if reports.is_empty() {
        return Err(format!(
            "no criterion change reports under {} — run `just bench-micro-baseline` first, \
             then `cargo bench -p fmf-core -- --baseline committed`",
            dir.display()
        )
        .into());
    }

    let mut regressed = false;
    let mut checked = 0u32;
    for path in &reports {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let Some(median) = v["median"]["point_estimate"].as_f64() else {
            continue;
        };
        checked += 1;
        // Bench id = the path between the criterion dir and /change/.
        let name = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.strip_prefix(dir).ok())
            .map(|p| p.display().to_string().replace('\\', "/"))
            .unwrap_or_else(|| path.display().to_string());
        if median > threshold {
            eprintln!("REGRESSION {name} median {:+.1}%", median * 100.0);
            regressed = true;
        }
    }
    println!(
        "criterion-gate: {checked} benches compared, threshold {:+.0}%",
        threshold * 100.0
    );
    if regressed {
        return Err("micro-benchmark regression vs criterion baseline".into());
    }
    Ok(())
}

fn stats(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive)?;
    // Mirror the engine's Ready state (offset table prewarmed) so the
    // accounting reflects what the app actually holds.
    query::prewarm(&idx);
    let mut s = idx.stats(drive);
    s.add_derived_bytes(query::derived_cache_bytes(&idx));
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}

fn diag() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "fmf {} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    );
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    println!(r"engine log : {program_data}\find-my-files\logs\engine.log");
    println!(r"app log    : %APPDATA%\find-my-files\logs\app.log");
    println!("log filter : FMF_LOG (env var, e.g. FMF_LOG=debug)");
    let errors = fmf_core::diag::recent_errors();
    println!("recent in-process diagnostics ({}):", errors.len());
    println!("{}", serde_json::to_string_pretty(&errors)?);
    Ok(())
}
