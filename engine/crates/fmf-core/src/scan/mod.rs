//! Streaming $MFT scanner (ADR-0011).
//!
//! The $MFT's data runs are read in 16MiB aligned chunks through our own
//! volume handle, records are fixed up and parsed per chunk, and the
//! buffers are recycled — peak RAM is bounded at a few chunks. Boot-sector,
//! record, and attribute bytes are decoded by the alignment-independent
//! parsers in [`crate::ondisk`]; untrusted disk bytes are never cast to Rust
//! references. This module owns only acquisition and orchestration, which is
//! why the grammar it drives is not gated to Windows with it (ADR-0047).
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

pub use probe::{IoProbeMode, ProbeStats, io_probe};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;

use crate::index::{Frn, VolumeIndex, VolumeIndexBuilder};
use crate::mft::{MftError, peak_working_set};
use crate::volume_label::VolumeLabel;

use deferred::{DeferredContext, DeferredError, resolve_deferred};
use parse::{RecordArena, append_batches, parse_chunk};
use pipeline::{PipelineOutcome, plan_chunks, run_chunk_pipeline};
use volume_io::mft_layout;
pub(crate) use volume_io::{SectorAlignedReader, open_raw_volume, volume_geometry};

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
    /// Builder finish: the name-permutation sort.
    pub elapsed_sort_ms: u64,
    /// 1 when the read-ahead I/O thread could not start and the scan
    /// degraded to inline sequential reads.
    pub pipeline_fallbacks: u64,
    /// Searchable file-link rows indexed.
    pub files: u64,
    /// Searchable directory-link rows indexed.
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
    /// Name/attribute-list-bearing extension records past the in-RAM cache
    /// cap (those targets fall back to disk reads in the deferred pass).
    /// Base/extension records spilled from the shared 128MiB deferred arena.
    /// Only their record numbers remain; the deferred pass reads them lazily.
    pub deferred_record_cache_spills: u64,
    /// Deferred-pass targeted MFT reads that failed before the authoritative
    /// live-metadata fallback ran.
    pub deferred_name_read_failures: u64,
    /// Deferred-pass objects the live metadata source could not size, so the
    /// row was published with the size its base record proves. Non-zero on
    /// every real volume: NTFS metadata files past `FIRST_NORMAL_RECORD`
    /// (`\$Extend\$ObjId` and friends) cannot be opened by file id. A large
    /// count means something else — sharing violations or a failing device.
    pub deferred_stat_failures: u64,
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
    scan_volume_cancellable(drive, &Arc::new(AtomicBool::new(false)))
}

/// Full initial scan with cooperative shutdown.
///
/// Cancellation is checked
/// between bounded raw reads, parsed chunks, deferred records, and builder
/// stages. A cancelled scan never returns or publishes a partial index.
///
/// # Errors
///
/// Returns [`MftError::Cancelled`] when `stop` is set, in addition to the
/// errors documented by [`scan_volume`].
pub fn scan_volume_cancellable(
    drive: &str,
    stop: &Arc<AtomicBool>,
) -> Result<(VolumeIndex, ScanStats), MftError> {
    if stop.load(Ordering::Relaxed) {
        return Err(MftError::Cancelled);
    }
    let label = VolumeLabel::parse(drive).ok_or_else(|| {
        MftError::Ntfs("volume label must be exactly one ASCII drive letter and ':'".to_string())
    })?;
    let drive = label.as_str();
    let volume_path = label.raw_path();
    let mut stats = ScanStats {
        volume: drive.to_string(),
        ..Default::default()
    };

    let t0 = Instant::now();
    let layout = mft_layout(&volume_path).map_err(MftError::from)?;
    if stop.load(Ordering::Relaxed) {
        return Err(MftError::Cancelled);
    }
    stats.mft_bytes = layout.data_size;

    let chunks =
        plan_chunks(&layout.runmap, layout.data_size, layout.record_size).ok_or_else(|| {
            MftError::Ntfs("$MFT length is not a whole number of file records".to_string())
        })?;
    let mut b = VolumeIndexBuilder::new_strict(drive, Frn(layout.root_reference))
        .map_err(|error| MftError::Ntfs(error.to_string()))?;
    let mut deferred: Vec<(u64, Option<u32>)> = Vec::new();
    let mut extensions: FxHashMap<u64, u32> = FxHashMap::default();
    let mut arena = RecordArena::new(layout.record_size);
    let mut parse_time = Duration::ZERO;
    let mut corrupt_records = 0u64;

    let pipeline = run_chunk_pipeline(&volume_path, &chunks, stop, &mut |i, bytes| {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let t = Instant::now();
        let batches = parse_chunk(
            bytes,
            chunks[i].logical,
            layout.record_size,
            layout.sector_size,
        );
        if !stop.load(Ordering::Relaxed) {
            corrupt_records += append_batches(
                &mut b,
                &mut stats,
                &mut deferred,
                &mut extensions,
                &mut arena,
                batches,
            );
        }
        parse_time += t.elapsed();
    })
    .map_err(MftError::from)?;
    let PipelineOutcome::Complete {
        read_time,
        fallbacks,
    } = pipeline
    else {
        return Err(MftError::Cancelled);
    };
    stats.elapsed_mft_load_ms = read_time.as_millis() as u64;
    stats.elapsed_parse_ms = parse_time.as_millis() as u64;
    stats.pipeline_fallbacks = fallbacks;
    if corrupt_records > 0 {
        return Err(MftError::CorruptRecords(corrupt_records));
    }
    tracing::debug!(
        area = "scan",
        volume = drive,
        msg = "scan phase: mft read complete"
    );

    // Deferred pass: names hiding behind $ATTRIBUTE_LIST, resolved in
    // parallel from the streamed extension-record cache (ADR-0011).
    let t_deferred = Instant::now();
    stats.deferred_names = deferred.len() as u64;
    let batches = if deferred.is_empty() {
        Vec::new()
    } else {
        tracing::debug!(
            area = "scan",
            volume = drive,
            objects = deferred.len(),
            msg = "scan phase: opening live metadata"
        );
        let metadata =
            crate::usn::MetadataSource::open_volume_cancellable(drive, Arc::clone(stop))?;
        tracing::debug!(
            area = "scan",
            volume = drive,
            msg = "scan phase: resolving deferred names"
        );
        resolve_deferred(
            DeferredContext {
                volume_path: &volume_path,
                runmap: &layout.runmap,
                record_size: layout.record_size,
                sector_size: layout.sector_size,
                cluster_size: layout.cluster_size,
                volume_size: layout.volume_size,
                extensions: &extensions,
                arena: &arena,
                metadata: &metadata,
                stop,
            },
            &deferred,
        )
        .map_err(|error| match error {
            DeferredError::Cancelled => MftError::Cancelled,
            DeferredError::Incomplete(reference) => MftError::IncompleteMetadata(reference),
        })?
    };
    corrupt_records += append_batches(
        &mut b,
        &mut stats,
        &mut Vec::new(),
        &mut FxHashMap::default(),
        &mut RecordArena::new(layout.record_size),
        batches,
    );
    debug_assert_eq!(
        corrupt_records, 0,
        "deferred batches are built only from already-validated records"
    );
    stats.elapsed_deferred_ms = t_deferred.elapsed().as_millis() as u64;
    tracing::debug!(
        area = "scan",
        volume = drive,
        ms = stats.elapsed_deferred_ms,
        msg = "scan phase: deferred complete"
    );
    drop(extensions);
    drop(deferred);
    drop(arena);
    // Shared-arena spills and failed targeted reads remain observable even
    // when the authoritative live fallback completed the object.

    tracing::debug!(
        area = "scan",
        volume = drive,
        msg = "scan phase: builder finish"
    );
    let Some((idx, finish)) = b
        .finish_timed_cancellable(stop)
        .map_err(|error| MftError::Ntfs(error.to_string()))?
    else {
        return Err(MftError::Cancelled);
    };
    stats.elapsed_build_ms = finish.build_ms;
    stats.elapsed_sort_ms = finish.sort_ms;
    stats.elapsed_total_ms = t0.elapsed().as_millis() as u64;
    stats.peak_working_set_bytes = peak_working_set();
    Ok((idx, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileged_scan_and_probe_reject_non_drive_paths_before_os_access() {
        for invalid in [r"\\.\C:", r"C:\", "C:/", "CC:", "1:", "../C:", " C:"] {
            assert!(matches!(scan_volume(invalid), Err(MftError::Ntfs(_))));
            assert!(matches!(
                io_probe(invalid, IoProbeMode::Buffered, 1),
                Err(MftError::Ntfs(_))
            ));
        }
    }

    /// Fails closed. `#[ignore]` is what *skips* this test; reaching the body
    /// without the arming variable means the harness was invoked outside
    /// `just test-admin`, and a silent early return would be indistinguishable
    /// from a real-volume run that actually happened.
    fn require_admin_gate() {
        assert_eq!(
            std::env::var("FMF_ADMIN_TESTS").as_deref(),
            Ok("1"),
            "this ignored real-volume test must run only through `just test-admin`"
        );
    }

    /// Cross-check the streamed raw-$MFT index against exact live-record
    /// lookups obtained through `FSCTL_GET_NTFS_FILE_RECORD`.  The two paths
    /// share only the checked byte grammar; their acquisition and traversal
    /// are independent.
    #[test]
    #[ignore = "requires elevation; gated by FMF_ADMIN_TESTS"]
    fn streaming_scan_matches_live_exact_records() {
        use crate::usn::apply::LinkSnapshot;

        require_admin_gate();
        let (index, _) = scan_volume("C:").expect("streaming scan");
        let live = crate::usn::MetadataSource::open_volume("C:").expect("live metadata source");
        let mut checked = 0u64;
        let mut matched = 0u64;
        let mut unavailable = 0u64;
        let mut mismatches: Vec<String> = Vec::new();
        for entry in (1..index.len() as u32).step_by(997) {
            checked += 1;
            let reference = index.frn(entry).0;
            let parent = index.parent(entry);
            if parent == crate::index::NO_PARENT {
                continue;
            }
            let parent_reference = index.frn(parent).0;
            match live.links(reference) {
                LinkSnapshot::Present(links) => {
                    let found = links.iter().any(|link| {
                        if link.parent_frn != parent_reference {
                            return false;
                        }
                        let mut original = Vec::new();
                        let mut folded = Vec::new();
                        crate::wtf8::push_wtf8_pair(&link.name, &mut original, &mut folded);
                        original == index.name(entry)
                    });
                    if found {
                        matched += 1;
                    } else if mismatches.len() < 16 {
                        mismatches.push(format!(
                            "FRN {reference} no longer has indexed parent/name `{}`",
                            String::from_utf8_lossy(index.name(entry)),
                        ));
                    }
                }
                LinkSnapshot::Gone | LinkSnapshot::Failed => unavailable += 1,
            }
        }
        assert!(checked > 100, "sample too small: {checked}");
        let comparable = checked.saturating_sub(unavailable);
        assert!(comparable > 100, "too few live records were comparable");
        assert!(
            matched as f64 / comparable as f64 > 0.999,
            "sampled live-link mismatch: {matched}/{comparable} ({unavailable} unavailable)\n{}",
            mismatches.join("\n")
        );
    }
}
