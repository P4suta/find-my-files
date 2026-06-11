//! `spike`, `index` (query REPL) and `watch` — the interactive volume tools
//! (all require an elevated terminal: they read the real $MFT/USN journal).

use std::io::{BufRead, Write};
use std::time::Instant;

use fmf_core::index::SortKey;
use fmf_core::query::{CaseMode, QueryOptions};

use super::{build_index, run_query};

pub fn spike(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
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

pub fn index(
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
pub fn watch(drive: &str) -> Result<(), Box<dyn std::error::Error>> {
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
