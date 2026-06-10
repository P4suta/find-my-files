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
        "indexed {} entries ({} files, {} dirs, {} skipped) in {} ms ($MFT load {} ms)",
        idx.len(),
        s.files,
        s.dirs,
        s.skipped_no_name,
        s.elapsed_total_ms,
        s.elapsed_mft_load_ms
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
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BenchReport {
    volume: String,
    entries: u64,
    peak_working_set_bytes: u64,
    queries: Vec<QueryBench>,
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
    };

    println!(
        "{:<28} {:>10} {:>9} {:>9} {:>9} | {:>8} {:>8} {:>8}",
        "query", "hits", "p50_us", "p99_us", "max_us", "memo", "scan", "mat"
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
        };
        println!(
            "{:<28} {:>10} {:>9} {:>9} {:>9} | {:>8} {:>8} {:>8}",
            qb.query,
            qb.hits,
            qb.p50_us,
            qb.p99_us,
            qb.max_us,
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

    if let Some(path) = json {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        eprintln!("report written to {}", path.display());
    }

    if let Some(path) = baseline {
        let old: BenchReport = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let mut regressed = false;
        for qb in &report.queries {
            let Some(prev) = old.queries.iter().find(|p| p.query == qb.query) else {
                continue;
            };
            // Ignore sub-200µs noise; flag >20% regressions.
            let gate = |new: u64, old: u64| new > old.max(200) + old.max(200) / 5;
            if gate(qb.p50_us, prev.p50_us) || gate(qb.p99_us, prev.p99_us) {
                eprintln!(
                    "REGRESSION {:<24} p50 {}→{}µs p99 {}→{}µs",
                    qb.query, prev.p50_us, qb.p50_us, prev.p99_us, qb.p99_us
                );
                regressed = true;
            }
        }
        if regressed {
            return Err("benchmark regression vs baseline".into());
        }
        eprintln!("no regression vs {}", path.display());
    }
    Ok(())
}

fn stats(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive)?;
    let s = idx.stats(drive);
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
