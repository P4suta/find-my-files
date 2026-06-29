//! `io-probe` — $MFT read-throughput measurement per I/O strategy
//! (verdicts: docs/RESEARCH.md / ADR-0011).

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ProbeModeArg {
    Buffered,
    Seq,
    Nobuf,
    NobufOv,
}

impl From<ProbeModeArg> for fmf_core::scan::IoProbeMode {
    fn from(m: ProbeModeArg) -> Self {
        use fmf_core::scan::IoProbeMode::{Buffered, NoBuf, NoBufOverlapped, Seq};
        match m {
            ProbeModeArg::Buffered => Buffered,
            ProbeModeArg::Seq => Seq,
            ProbeModeArg::Nobuf => NoBuf,
            ProbeModeArg::NobufOv => NoBufOverlapped,
        }
    }
}

pub fn io_probe(
    drive: &str,
    mode: ProbeModeArg,
    qd: usize,
    runs: usize,
    ctx: super::ctx::Ctx,
) -> Result<(), Box<dyn std::error::Error>> {
    let runs = runs.max(1);
    let mut measured = Vec::with_capacity(runs);
    for run in 0..runs {
        let s = fmf_core::scan::io_probe(drive, mode.into(), qd)?;
        if ctx.human_chrome() {
            println!(
                "run {run}: {:>7.1} MB/s  ({:.1} MiB in {} ms, mode {mode:?}, qd {qd})",
                s.mb_per_s,
                s.bytes as f64 / f64::from(1 << 20),
                s.elapsed_ms
            );
        }
        measured.push((s.mb_per_s, s.bytes, s.elapsed_ms));
    }
    let mut rates: Vec<f64> = measured.iter().map(|m| m.0).collect();
    rates.sort_by(f64::total_cmp);
    let median = rates[rates.len() / 2];

    if ctx.is_json() {
        let runs_json: Vec<_> = measured
            .iter()
            .enumerate()
            .map(|(run, &(mb_per_s, bytes, elapsed_ms))| {
                serde_json::json!({
                    "run": run,
                    "mb_per_s": mb_per_s,
                    "bytes": bytes,
                    "elapsed_ms": elapsed_ms,
                })
            })
            .collect();
        super::json::emit(&serde_json::json!({
            "mode": format!("{mode:?}"),
            "qd": qd,
            "runs": runs_json,
            "median_mb_per_s": median,
        }))?;
    } else {
        println!("median: {median:.1} MB/s");
    }
    Ok(())
}
