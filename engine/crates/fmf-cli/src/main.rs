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
    Bench { drive: String },
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let result = match cli.command {
        Command::Spike { drive } => spike(&drive),
        Command::Index {
            drive,
            stats,
            include_hidden_system,
        } => index(&drive, stats, include_hidden_system),
        Command::Bench { drive } => bench(&drive),
        Command::Watch { drive } => watch(&drive),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
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
) -> Result<query::SearchResult, Box<dyn std::error::Error>> {
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
            Ok(r) => {
                let elapsed = t.elapsed();
                let mut path = Vec::new();
                for &id in r.ids.iter().take(20) {
                    path.clear();
                    idx.append_path(id, &mut path);
                    out.write_all(&path)?;
                    out.write_all(b"\n")?;
                }
                println!("-- {} hits in {:?}", r.ids.len(), elapsed);
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
            ReadOutcome::Records(rs) => {
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
    "a",                        // worst case: 1 char, huge hit count
    "win",                      // common 3-char substring
    "qzx",                      // rare substring
    "ext:dll",                  // extension filter
    "size:>100mb path:windows", // composite
    "*.rs",                     // wildcard
];

fn bench(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
    let idx = build_index(drive)?;
    let opt = QueryOptions::default();

    println!(
        "{:<28} {:>10} {:>10} {:>10} {:>10}",
        "query", "hits", "p50_us", "p99_us", "max_us"
    );
    for q in BENCH_QUERIES {
        const RUNS: usize = 50;
        let mut times = Vec::with_capacity(RUNS);
        let mut hits = 0usize;
        for _ in 0..RUNS {
            let t = Instant::now();
            let r = run_query(&idx, q, opt)?;
            times.push(t.elapsed().as_micros() as u64);
            hits = r.ids.len();
        }
        times.sort_unstable();
        println!(
            "{:<28} {:>10} {:>10} {:>10} {:>10}",
            q,
            hits,
            times[RUNS / 2],
            times[RUNS * 99 / 100],
            times[RUNS - 1]
        );
    }
    println!(
        "peak working set {:.1} MiB",
        fmf_core::mft::peak_working_set() as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}
