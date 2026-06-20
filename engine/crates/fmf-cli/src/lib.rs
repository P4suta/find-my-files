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
#[command(name = "fmf", version, about = "find-my-files engine developer CLI")]
struct Cli {
    /// When to colourise human-facing output (auto: only on a terminal).
    #[arg(long, value_enum, default_value_t = ColorArg::Auto, global = true)]
    color: ColorArg,
    /// Suppress the progress spinner and other stderr chrome.
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Output format. `json` emits a machine-readable document on stdout for
    /// the commands that support it (diag, bench, watch); others stay text.
    #[arg(long, value_enum, default_value_t = Format::Human, global = true)]
    format: Format,
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
    Stats {
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
    },
    /// Measure $MFT read throughput per I/O strategy (elevated terminal;
    /// reads the scan's exact chunk plan, parses nothing). Verdicts live in
    /// docs/RESEARCH.md — production reads stay buffered until a mode wins.
    IoProbe {
        drive: String,
        #[arg(long, value_enum, default_value_t = ProbeModeArg::Buffered)]
        mode: ProbeModeArg,
        /// Outstanding reads for nobuf-ov.
        #[arg(long, default_value_t = 4)]
        qd: usize,
        #[arg(long, default_value_t = 3)]
        runs: usize,
    },
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

/// The clap command tree for `fmf` — the single definition behind argv parsing,
/// the generated shell completions, and the generated CLI reference.
#[must_use]
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse argv, run the requested subcommand, and exit the process with the
/// mapped `FMF_E_*` code on failure. Returns normally on success.
pub fn run() {
    // Same pipeline as the DLL: stderr log + diag ring + panic capture.
    fmf_core::diag::init_diag(None, "info");

    let cli = Cli::parse();
    let color = cmd::term::resolve_color(cli.color);
    // Make the choice global so the styled anstream macros pick it up.
    color.write_global();
    let ctx = cmd::ctx::Ctx {
        quiet: cli.quiet,
        format: cli.format,
    };
    let result = match cli.command {
        Command::Spike { drive } => cmd::index::spike(&drive),
        Command::Index {
            drive,
            stats,
            include_hidden_system,
        } => cmd::index::index(&drive, stats, include_hidden_system, ctx),
        Command::Bench {
            drive,
            json,
            baseline,
        } => cmd::bench::bench(&drive, json.as_deref(), baseline.as_deref(), ctx),
        Command::Stats {
            drive,
            trigram_estimate,
            name_stats,
        } => cmd::stats::stats(&drive, trigram_estimate, name_stats, ctx),
        Command::IoProbe {
            drive,
            mode,
            qd,
            runs,
        } => cmd::io_probe::io_probe(&drive, mode, qd, runs),
        Command::Diag => cmd::diag::diag(ctx),
        Command::Watch { drive } => cmd::index::watch(&drive, ctx),
        Command::CriterionGate { dir, threshold } => {
            cmd::criterion_gate::criterion_gate(&dir, threshold)
        }
    };
    if let Err(e) = result {
        let code = cmd::exit::report(e.as_ref(), color, ctx.format);
        std::process::exit(code);
    }
}
