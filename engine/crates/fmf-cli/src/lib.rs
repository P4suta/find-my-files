//! fmf — developer CLI for the find-my-files engine.
//!
//! The crate is a thin clap surface over fmf-core: [`command`] exposes the
//! parser (so codegen can render shell completions and the CLI reference) and
//! [`run`] parses argv and dispatches. Command implementations live in `cmd/`
//! (and `bench_support` for the bench-shared pieces); all engine logic stays in
//! fmf-core.

mod bench_support;
mod cmd;

use clap::{CommandFactory, Parser, Subcommand};

use crate::cmd::ctx::Format;
use crate::cmd::io_probe::ProbeModeArg;
use crate::cmd::term::ColorArg;

#[derive(Parser)]
#[command(
    name = "fmf",
    version = fmf_buildstamp::VERSION,
    about = "find-my-files engine developer CLI",
    long_about = "find-my-files engine developer CLI — index, benchmark and diagnose the \
                  Rust search engine from a terminal.\n\n\
                  This is a developer / diagnostic tool; end-user search lives in the WinUI \
                  app (see the project README). Commands that read the $MFT/USN need an \
                  elevated terminal.",
    after_help = "EXAMPLES:\n  \
        fmf index C: --stats           # index C: and print memory stats (no REPL)\n  \
        fmf bench C: --out report.json # run the benchmark set, save a JSON report\n  \
        fmf stats C: --format json     # machine-readable per-column accounting\n  \
        fmf -v diag                    # diagnostics with debug-level logging\n  \
        fmf completions powershell     # print a PowerShell completion script",
)]
struct Cli {
    /// When to colourise human-facing output (auto: only on a terminal).
    #[arg(long, value_enum, default_value_t = ColorArg::Auto, global = true)]
    color: ColorArg,
    /// Suppress the progress spinner and other stderr chrome.
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Increase log verbosity: `-v` = debug, `-vv` = trace (default: info). The
    /// `FMF_LOG` environment variable still takes precedence when set.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
    /// Output format. `json` emits a single machine-readable document on stdout
    /// (NDJSON for the streaming `watch`); the interactive `index` REPL and
    /// `completions` are text-only.
    #[arg(long, value_enum, default_value_t = Format::Human, global = true)]
    format: Format,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Spike S0: scan a volume's $MFT and print raw measurements.
    Spike {
        /// Volume to scan, e.g. `C:` (an NTFS drive root).
        drive: String,
    },
    /// Build the index for a volume, print stats, then run an interactive
    /// query REPL (requires an elevated terminal).
    Index {
        /// Volume to index, e.g. `C:` (an NTFS drive root).
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
        /// Volume to index and benchmark, e.g. `C:` (an NTFS drive root).
        drive: String,
        /// Write the full report to this file as JSON (distinct from the global
        /// `--format json`, which streams a document to stdout).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Compare against a previous `--out` report; exit 1 when p50 or p99
        /// regress by more than 20%.
        #[arg(long)]
        baseline: Option<std::path::PathBuf>,
    },
    /// Index a volume and dump per-column memory accounting as JSON.
    Stats {
        /// Volume to index and measure, e.g. `C:` (an NTFS drive root).
        drive: String,
        /// Also estimate what a trigram index would cost on this volume's
        /// real names — input to the n-gram go/no-go criteria in
        /// docs/ARCHITECTURE.md (read-only, nothing is built).
        #[arg(long)]
        trigram_estimate: bool,
        /// Also dump per-name statistics over the live entries (fold
        /// identity, duplication, length distribution, ≥4GiB sizes) — the
        /// measured inputs for pool/column layout decisions (read-only).
        #[arg(long)]
        name_stats: bool,
        /// Also estimate the name-dictionary-encoding memory delta on this
        /// volume's real names: store each distinct folded name once and
        /// replace `name_off`+`name_len` with a `name_id` — the Phase-2
        /// go/no-go input (read-only, nothing is built).
        #[arg(long)]
        dict_estimate: bool,
    },
    /// Measure $MFT read throughput per I/O strategy (elevated terminal;
    /// reads the scan's exact chunk plan, parses nothing). Verdicts live in
    /// docs/RESEARCH.md — production reads stay buffered until a mode wins.
    IoProbe {
        /// Volume to probe, e.g. `C:` (an NTFS drive root).
        drive: String,
        /// I/O strategy to measure (buffered, unbuffered, overlapped …).
        #[arg(long, value_enum, default_value_t = ProbeModeArg::Buffered)]
        mode: ProbeModeArg,
        /// Outstanding reads for nobuf-ov.
        #[arg(long, default_value_t = 4)]
        qd: usize,
        /// Number of timed runs to take the best of.
        #[arg(long, default_value_t = 3)]
        runs: usize,
    },
    /// Print versions, log locations and the in-process diagnostics ring.
    Diag,
    /// Index a volume, then tail its USN journal and apply changes live,
    /// printing one line per applied batch (Ctrl+C to stop).
    Watch {
        /// Volume to index and watch, e.g. `C:` (an NTFS drive root).
        drive: String,
    },
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
    /// Print a shell completion script to stdout (bash/zsh/fish/PowerShell/elvish).
    /// Add it with e.g. `eval "$(fmf completions bash)"`, or use the pre-generated
    /// scripts shipped under `completions/` in the release bundle.
    Completions {
        /// The shell to generate the completion script for.
        shell: clap_complete::Shell,
    },
}

/// The clap command tree for `fmf` — the single definition behind argv parsing,
/// the generated shell completions, and the generated CLI reference.
#[must_use]
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse argv, run the requested subcommand, and exit the process with the
/// mapped `FMF_E_*` code on failure. Returns normally on success.
pub fn run() {
    let cli = Cli::parse();
    // -v/--verbose raises the default level; FMF_LOG still overrides it (init_diag).
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    // Same pipeline as the DLL: stderr log + diag ring + panic capture.
    fmf_core::diag::init_diag(None, level, fmf_core::diag::DEFAULT_MAX_LOG_FILES);

    let color = cmd::term::resolve_color(cli.color);
    // Make the choice global so the styled anstream macros pick it up.
    color.write_global();
    let ctx = cmd::ctx::Ctx {
        quiet: cli.quiet,
        format: cli.format,
    };
    let result = match cli.command {
        Command::Spike { drive } => cmd::index::spike(&drive, ctx),
        Command::Index {
            drive,
            stats,
            include_hidden_system,
        } => cmd::index::index(&drive, stats, include_hidden_system, ctx),
        Command::Bench {
            drive,
            out,
            baseline,
        } => cmd::bench::bench(&drive, out.as_deref(), baseline.as_deref(), ctx),
        Command::Stats {
            drive,
            trigram_estimate,
            name_stats,
            dict_estimate,
        } => cmd::stats::stats(&drive, trigram_estimate, name_stats, dict_estimate, ctx),
        Command::IoProbe {
            drive,
            mode,
            qd,
            runs,
        } => cmd::io_probe::io_probe(&drive, mode, qd, runs, ctx),
        Command::Diag => cmd::diag::diag(ctx),
        Command::Watch { drive } => cmd::index::watch(&drive, ctx),
        Command::CriterionGate { dir, threshold } => {
            cmd::criterion_gate::criterion_gate(&dir, threshold, ctx)
        }
        Command::Completions { shell } => {
            cmd::completions::completions(shell);
            Ok(())
        }
    };
    if let Err(e) = result {
        let code = cmd::exit::report(e.as_ref(), color, ctx.format);
        std::process::exit(code);
    }
}
