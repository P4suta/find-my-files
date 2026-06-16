//! Streaming $MFT scanner (ADR-0011).
//!
//! The $MFT's data runs are read in 16MiB aligned chunks through our own
//! volume handle, records are fixed up and parsed per chunk, and the
//! buffers are recycled — peak RAM is bounded at a few chunks. ntfs-reader
//! provides the bootstrap (boot-sector geometry + record 0's data runs) and
//! the per-record attribute parsing types.
//!
//! Two layers of overlap (entry order stays byte-for-byte identical to a
//! sequential scan):
//! - a dedicated I/O thread reads chunk N+1 while chunk N parses
//!   (`pipeline::run_chunk_pipeline`; degrades to inline reads if the
//!   thread can't start — `scan_pipeline_fallbacks`)
//! - within a chunk, record sub-ranges parse on rayon workers that carry
//!   the WTF-8 encoding too (`parse::parse_chunk`); the builder then
//!   appends the worker batches in chunk order, so `EntryId` assignment is
//!   deterministic.

mod deferred;
mod parse;
mod pipeline;
mod probe;
mod volume_io;
pub mod walk;
mod walk_id;

pub use probe::{IoProbeMode, ProbeStats, io_probe};

use std::time::{Duration, Instant};

use ntfs_reader::api::ROOT_RECORD;
use ntfs_reader::errors::NtfsReaderError;
use rustc_hash::FxHashMap;

use crate::index::{VolumeIndex, VolumeIndexBuilder};
use crate::mft::{MftError, peak_working_set};

use deferred::resolve_deferred;
use parse::{RecordArena, append_batches, parse_chunk};
use pipeline::{plan_chunks, run_chunk_pipeline};
use volume_io::mft_layout;

/// Statistics from a full index build.
#[derive(Debug, Default)]
pub struct ScanStats {
    /// Drive letter spec that was scanned (e.g. `C:`).
    pub volume: String,
    /// Wall-clock time for the whole scan + build (ms).
    pub elapsed_total_ms: u64,
    /// Accumulated device-read time. Overlaps with parsing on the pipelined
    /// path, so read + parse + build + sort may exceed total.
    pub elapsed_mft_load_ms: u64,
    /// Accumulated record-parse time (fixup + attribute walk + WTF-8).
    pub elapsed_parse_ms: u64,
    /// Deferred $`ATTRIBUTE_LIST` name resolution.
    pub elapsed_deferred_ms: u64,
    /// Records whose name needed the deferred pass at all.
    pub deferred_names: u64,
    /// Builder finish: parent resolution + EXCLUDED propagation.
    pub elapsed_build_ms: u64,
    /// Builder finish: the three permutation sorts.
    pub elapsed_sort_ms: u64,
    /// 1 when the read-ahead I/O thread could not start and the scan
    /// degraded to inline sequential reads.
    pub pipeline_fallbacks: u64,
    /// Files indexed (count).
    pub files: u64,
    /// Directories indexed (count).
    pub dirs: u64,
    /// Records dropped because no usable name could be resolved (count).
    pub skipped_no_name: u64,
    /// Peak working-set RAM of the scanning process (bytes).
    pub peak_working_set_bytes: u64,
    /// Raw $MFT size — the bytes the initial scan reads.
    pub mft_bytes: u64,
    /// Extension records (`base_reference` != 0) — parts of other files,
    /// correctly not indexed standalone.
    pub extension_records: u64,
    /// Records failing signature/fixup validation.
    pub corrupt_records: u64,
    /// Deferred $`ATTRIBUTE_LIST` records whose name never resolved.
    pub deferred_unresolved: u64,
    /// Name-bearing extension records past the in-RAM cache cap (those
    /// targets fall back to disk reads in the deferred pass).
    pub ext_name_cache_skipped: u64,
    /// Deferred-pass targeted disk reads that failed — each one is a name
    /// that stays unresolved until the next rescan.
    pub deferred_name_read_failures: u64,
    /// Scope-mode (folder-walk, ADR-0024) only: directories enumerated.
    pub walk_dirs: u64,
    /// Scope-mode only: files enumerated.
    pub walk_files: u64,
    /// Scope-mode only: wall-clock of the enumeration phase (ms).
    pub elapsed_walk_ms: u64,
    /// Scope-mode only: roots/dirs/entries skipped because they could not be
    /// read (permission, vanished). The worker maps this to a counter + warn.
    pub walk_read_errors: u64,
    /// Scope-mode only: subtrees not descended because they hit `MAX_DEPTH`.
    pub walk_depth_truncated: u64,
}

/// Full initial scan: stream the volume's $MFT and build the in-memory
/// index. `drive` is a drive letter spec like `C:`.
///
/// # Errors
///
/// Returns [`MftError::NotElevated`] when the process lacks the privileges to
/// open the raw volume, or [`MftError::Ntfs`] if opening the volume or
/// reading the $MFT fails.
pub fn scan_volume(drive: &str) -> Result<(VolumeIndex, ScanStats), MftError> {
    let drive = drive.trim_end_matches(['\\', '/']);
    let volume_path = format!(r"\\.\{drive}");
    let mut stats = ScanStats {
        volume: drive.to_string(),
        ..Default::default()
    };

    let t0 = Instant::now();
    let (record_size, data_size, runmap) = mft_layout(&volume_path).map_err(|e| match e {
        NtfsReaderError::ElevationError => MftError::NotElevated,
        other => MftError::Ntfs(other),
    })?;
    stats.mft_bytes = data_size;

    let chunks = plan_chunks(&runmap, data_size, record_size);
    let mut b = VolumeIndexBuilder::new(drive, ROOT_RECORD);
    let mut deferred: Vec<(u64, u32)> = Vec::new();
    let mut extensions: FxHashMap<u64, u32> = FxHashMap::default();
    let mut arena = RecordArena::new(record_size);
    let mut parse_time = Duration::ZERO;

    let (read_time, fallbacks) = run_chunk_pipeline(&volume_path, &chunks, &mut |i, bytes| {
        let t = Instant::now();
        let batches = parse_chunk(bytes, chunks[i].logical, record_size);
        append_batches(
            &mut b,
            &mut stats,
            &mut deferred,
            &mut extensions,
            &mut arena,
            batches,
        );
        parse_time += t.elapsed();
    })
    .map_err(MftError::Ntfs)?;
    stats.elapsed_mft_load_ms = read_time.as_millis() as u64;
    stats.elapsed_parse_ms = parse_time.as_millis() as u64;
    stats.pipeline_fallbacks = fallbacks;

    // Deferred pass: names hiding behind $ATTRIBUTE_LIST, resolved in
    // parallel from the streamed extension-record cache (ADR-0011).
    let t_deferred = Instant::now();
    stats.deferred_names = deferred.len() as u64;
    let batches = resolve_deferred(
        &volume_path,
        &runmap,
        record_size,
        &extensions,
        &arena,
        &deferred,
    );
    append_batches(
        &mut b,
        &mut stats,
        &mut Vec::new(),
        &mut FxHashMap::default(),
        &mut RecordArena::new(record_size),
        batches,
    );
    stats.elapsed_deferred_ms = t_deferred.elapsed().as_millis() as u64;
    drop(extensions);
    drop(deferred);
    drop(arena);
    // Cache overflow (`ext_name_cache_skipped`) and failed deferred reads
    // (`deferred_name_read_failures`) are returned in ScanStats only; the
    // volume worker maps them into counters + warn at its single mapping
    // point (engine/worker.rs).

    // Degradations are normal in small numbers; make them visible either way.
    if stats.corrupt_records > 0 {
        tracing::warn!(volume = %drive, count = stats.corrupt_records, "corrupt MFT records skipped");
    }
    if stats.deferred_unresolved > 0 {
        tracing::warn!(
            volume = %drive,
            count = stats.deferred_unresolved,
            "attribute-list names unresolved"
        );
    }

    let (idx, finish) = b.finish_timed();
    stats.elapsed_build_ms = finish.build_ms;
    stats.elapsed_sort_ms = finish.sort_ms;
    stats.elapsed_total_ms = t0.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = peak_working_set();
    Ok((idx, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equivalence gate against the whole-load reference path. Run from an
    /// elevated shell: `FMF_ADMIN_TESTS=1` cargo test -- --ignored streaming
    /// The volume is live, so a small drift tolerance is allowed.
    #[test]
    #[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
    fn streaming_scan_matches_reference() {
        if std::env::var("FMF_ADMIN_TESTS").as_deref() != Ok("1") {
            eprintln!("FMF_ADMIN_TESTS != 1 — skipping");
            return;
        }
        let (new_idx, new_stats) = scan_volume("C:").expect("streaming scan");
        let (old_idx, old_stats) = crate::mft::scan_volume_reference("C:").expect("reference");

        let drift = (new_idx.len() as i64 - old_idx.len() as i64).unsigned_abs();
        assert!(
            drift < old_idx.len() as u64 / 500,
            "entry counts diverged: streaming {} vs reference {} (files {}/{} dirs {}/{})",
            new_idx.len(),
            old_idx.len(),
            new_stats.files,
            old_stats.files,
            new_stats.dirs,
            old_stats.dirs,
        );

        // Sampled records must agree on name and size where both saw them.
        // Reparse points are excluded: pick_name keeps their names on
        // purpose while the reference's get_best_file_name skips them, so
        // the two resolvers legitimately disagree there (and on this class
        // only — see the module docs of `pick_name`).
        let mut checked = 0u64;
        let mut matched = 0u64;
        let mut size_matched = 0u64;
        let mut mismatches: Vec<String> = Vec::new();
        for sample in (0..old_idx.len() as u32).step_by(997) {
            let old_rec = old_idx.frn(sample).record();
            let (Some(o), Some(n)) = (
                old_idx.entry_by_record(old_rec),
                new_idx.entry_by_record(old_rec),
            ) else {
                continue;
            };
            if old_idx.is_reparse(o) || new_idx.is_reparse(n) {
                continue;
            }
            checked += 1;
            if old_idx.name(o) == new_idx.name(n) {
                matched += 1;
            } else {
                use std::os::windows::ffi::OsStringExt;

                // The resolvers legitimately disagree on attribute-list
                // names: get_best_file_name returns the *first* $FILE_NAME
                // of a target record (often the DOS 8.3 short name) and the
                // first Win32 link of hardlinked files, while pick_name
                // scans for the best Win32 name. Arbitrate with the disk:
                // if the streaming-derived full path exists, the streaming
                // name is right.
                let mut p = Vec::new();
                new_idx.append_path(n, &mut p);
                let mut units = Vec::new();
                crate::wtf8::wtf8_to_utf16(&p, &mut units);
                let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&units));
                if std::fs::symlink_metadata(&path).is_ok() {
                    matched += 1;
                } else if mismatches.len() < 16 {
                    mismatches.push(format!(
                        "record {}: reference `{}` vs streaming `{}` (path gone: {})",
                        old_rec.0,
                        String::from_utf8_lossy(old_idx.name(o)),
                        String::from_utf8_lossy(new_idx.name(n)),
                        path.display(),
                    ));
                }
            }
            if old_idx.size(o) == new_idx.size(n) {
                size_matched += 1;
            }
        }
        assert!(checked > 100, "sample too small: {checked}");
        assert!(
            matched as f64 / checked as f64 > 0.999,
            "sampled name mismatch: {matched}/{checked}\n{}",
            mismatches.join("\n")
        );
        // Sizes drift legitimately: the volume is live and the two scans run
        // a minute apart, so actively-written files differ. Names only move
        // on renames — hence the looser size bar.
        assert!(
            size_matched as f64 / checked as f64 > 0.99,
            "sampled size mismatch: {size_matched}/{checked}"
        );
    }
}
