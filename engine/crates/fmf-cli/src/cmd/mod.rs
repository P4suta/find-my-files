//! Subcommand implementations — one module per `fmf` subcommand family.
//! Logic lives in fmf-core; these are drivers and printers only.

pub mod bench;
pub mod criterion_gate;
pub mod diag;
pub mod exit;
pub mod index;
pub mod io_probe;
pub mod stats;
pub mod term;

use fmf_core::index::VolumeIndex;
use fmf_core::query::{self, QueryOptions};

#[cfg(windows)]
fn date_resolver() -> impl query::DateResolver {
    query::WindowsLocalResolver
}
#[cfg(not(windows))]
fn date_resolver() -> impl query::DateResolver {
    query::UtcResolver
}

fn build_index(drive: &str) -> Result<VolumeIndex, Box<dyn std::error::Error>> {
    let (mut idx, s) = fmf_core::mft::scan_volume(drive)?;
    // Mirror the engine (volume thread does the same): the build leaves
    // power-of-two capacity slack that would distort every RAM number.
    idx.shrink_to_fit();
    let per_entry = if idx.is_empty() {
        0
    } else {
        s.peak_working_set_bytes / idx.len() as u64
    };
    eprintln!(
        "indexed {} entries ({} files, {} dirs, {} skipped) in {} ms ($MFT read {} ms, parse {} ms, deferred {} names {} ms, build {} ms, sort {} ms — read/parse overlap)",
        idx.len(),
        s.files,
        s.dirs,
        s.skipped_no_name,
        s.elapsed_total_ms,
        s.elapsed_mft_load_ms,
        s.elapsed_parse_ms,
        s.deferred_names,
        s.elapsed_deferred_ms,
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
