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
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rates = Vec::with_capacity(runs.max(1));
    for run in 0..runs.max(1) {
        let s = fmf_core::scan::io_probe(drive, mode.into(), qd)?;
        println!(
            "run {run}: {:>7.1} MB/s  ({:.1} MiB in {} ms, mode {mode:?}, qd {qd})",
            s.mb_per_s,
            s.bytes as f64 / f64::from(1 << 20),
            s.elapsed_ms
        );
        rates.push(s.mb_per_s);
    }
    rates.sort_by(f64::total_cmp);
    println!("median: {:.1} MB/s", rates[rates.len() / 2]);
    Ok(())
}
