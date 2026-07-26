//! Deferred $`ATTRIBUTE_LIST` name resolution (ADR-0011): name-bearing
//! extension records are cached in RAM while the $MFT streams through, so
//! this pass resolves names without disk reads; anything missing (cache
//! cap, torn records) falls back to a targeted read of the live volume.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ntfs_reader::api::{NtfsAttributeType, NtfsFileName, NtfsFileNamespace};
use ntfs_reader::file::NtfsFile;
use rustc_hash::{FxHashMap, FxHashSet};

use super::attribute_list::{
    ListEntry, ListStreamError, StreamRun, close_extent_runs, covered_prefix, decode_extent_runs,
    parse_list_entries, visit_list_stream,
};
use super::parse::{ParsedBatch, RecordArena};
use super::record::attributes_complete;
use super::volume_io::{RunMap, apply_fixup, open_raw_volume};
use crate::mft::is_searchable_namespace;
use crate::usn::MetadataSource;
use crate::usn::apply::LinkSnapshot;

/// Hard byte ceiling shared by cached attribute-list base records and
/// name-bearing extension records. A normal NTFS record is 1KiB, but using a
/// byte bound (rather than a record count) keeps transient RAM ≤128MiB on
/// volumes with larger records too. Spills retain only the record number and
/// are read on demand during the deferred pass.
pub(super) const DEFERRED_RECORD_ARENA_MAX_BYTES: usize = 128 << 20;

pub(super) struct DeferredContext<'a> {
    pub(super) volume_path: &'a str,
    pub(super) runmap: &'a RunMap,
    pub(super) record_size: usize,
    pub(super) cluster_size: u64,
    pub(super) volume_size: u64,
    pub(super) extensions: &'a FxHashMap<u64, u32>,
    pub(super) arena: &'a RecordArena,
    pub(super) metadata: &'a MetadataSource,
    pub(super) stop: &'a Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeferredError {
    Cancelled,
    Incomplete(u64),
}

#[derive(Clone, Copy)]
struct ResolveSources<'a> {
    extensions: &'a FxHashMap<u64, u32>,
    arena: &'a RecordArena,
    cluster_size: u64,
    volume_size: u64,
    stop: &'a AtomicBool,
}

/// Random access to single records for the deferred attribute-list pass.
struct RecordReader<'a> {
    file: std::fs::File,
    map: &'a RunMap,
    record_size: usize,
    buf: Vec<u8>,
}

impl RecordReader<'_> {
    fn read_record(&mut self, number: u64) -> Option<&[u8]> {
        let logical = number.checked_mul(self.record_size as u64)?;
        self.buf.resize(self.record_size, 0);
        self.map
            .read_exact_logical(&mut self.file, logical, &mut self.buf)
            .ok()?;
        if !NtfsFile::is_valid(&self.buf)
            || !apply_fixup(&mut self.buf)
            || !attributes_complete(&self.buf)
        {
            return None;
        }
        Some(&self.buf)
    }
}

/// Disk fallback for extension records missing from the streamed cache —
/// opened only when actually needed (expected: never on a healthy scan).
struct LazyRecordReader<'a> {
    volume_path: &'a str,
    map: &'a RunMap,
    record_size: usize,
    inner: Option<RecordReader<'a>>,
    failed: bool,
    /// Failed `read_record` calls — each one is a name that stays
    /// unresolved until the next rescan. `resolve_deferred` folds this into
    /// its batch, so the count reaches `ScanStats` (don't go silent).
    failures: u64,
}

impl<'a> LazyRecordReader<'a> {
    const fn new(volume_path: &'a str, map: &'a RunMap, record_size: usize) -> Self {
        LazyRecordReader {
            volume_path,
            map,
            record_size,
            inner: None,
            failed: false,
            failures: 0,
        }
    }

    fn read_record(&mut self, number: u64) -> Option<&[u8]> {
        if self.inner.is_none() && !self.failed {
            match open_raw_volume(self.volume_path) {
                Ok(file) => {
                    self.inner = Some(RecordReader {
                        file,
                        map: self.map,
                        record_size: self.record_size,
                        buf: Vec::new(),
                    });
                }
                Err(_) => {
                    self.failed = true;
                }
            }
        }
        let Some(inner) = self.inner.as_mut() else {
            self.failures += 1;
            return None;
        };
        let got = inner.read_record(number);
        if got.is_none() {
            self.failures += 1;
        }
        got
    }
}

/// Lazily opened raw-volume reader for non-resident attribute-list streams.
/// One instance is reused by a rayon chunk, so the normal resident path opens
/// no extra handle.
struct LazyStreamReader<'a> {
    volume_path: &'a str,
    inner: Option<std::fs::File>,
    failed: bool,
    failures: u64,
}

impl<'a> LazyStreamReader<'a> {
    const fn new(volume_path: &'a str) -> Self {
        Self {
            volume_path,
            inner: None,
            failed: false,
            failures: 0,
        }
    }

    fn visit(
        &mut self,
        runs: &[StreamRun],
        data_size: u64,
        stop: &AtomicBool,
        prefix: bool,
        visit: impl FnMut(ListEntry),
    ) -> Option<()> {
        if stop.load(Ordering::Relaxed) || covered_prefix(runs) < data_size {
            return None;
        }
        if data_size == 0 {
            return Some(());
        }
        if self.inner.is_none() && !self.failed {
            match open_raw_volume(self.volume_path) {
                Ok(file) => self.inner = Some(file),
                Err(_) => {
                    self.failed = true;
                }
            }
        }
        let Some(file) = self.inner.as_mut() else {
            self.failures += 1;
            return None;
        };
        match visit_list_stream(file, runs, data_size, stop, prefix, visit) {
            Ok(()) => Some(()),
            Err(ListStreamError::Io) => {
                self.failures += 1;
                None
            }
            Err(ListStreamError::Invalid | ListStreamError::Cancelled) => None,
        }
    }
}

fn file_matches_reference(file: &NtfsFile<'_>, expected: u64) -> bool {
    file.reference_number() == expected
}

const fn extension_belongs_to(file: &NtfsFile<'_>, base_reference: u64) -> bool {
    let actual = file.header.base_reference;
    actual == base_reference
}

fn decode_extension_extent(
    number: u64,
    bytes: &[u8],
    entry: ListEntry,
    expected_base_reference: Option<u64>,
    cluster_size: u64,
    volume_size: u64,
) -> Option<Vec<StreamRun>> {
    let file = NtfsFile::new(number, bytes);
    if !file_matches_reference(&file, entry.target_reference)
        || expected_base_reference
            .is_some_and(|base_reference| !extension_belongs_to(&file, base_reference))
    {
        return None;
    }
    let mut found = None;
    file.attributes(|attr| {
        if found.is_some()
            || attr.header.type_id != NtfsAttributeType::AttributeList as u32
            || attr.header.id != entry.id
            || attr.header.is_non_resident == 0
        {
            return;
        }
        let Some(header) = attr.nonresident_header() else {
            return;
        };
        if u64::try_from(header.lowest_vcn).ok() != Some(entry.starting_vcn) {
            return;
        }
        found = decode_extent_runs(attr, cluster_size, volume_size).map(|(_, runs)| runs);
    });
    found
}

enum AttributeListSource<'a> {
    Resident(&'a [u8]),
    NonResident {
        data_size: u64,
        runs: Vec<StreamRun>,
    },
}

fn load_attribute_list<'a>(
    base: &'a NtfsFile<'a>,
    sources: &ResolveSources<'a>,
    record_reader: &mut LazyRecordReader<'_>,
    stream_reader: &mut LazyStreamReader<'_>,
) -> Option<AttributeListSource<'a>> {
    let attr = base.get_attribute(NtfsAttributeType::AttributeList)?;
    if attr.header.is_non_resident == 0 {
        return attr.get_resident().map(AttributeListSource::Resident);
    }

    let base_reference = base.reference_number();
    let base_attr_id = attr.header.id;
    let base_lowest_vcn = u64::try_from(attr.nonresident_header()?.lowest_vcn).ok()?;
    let (data_size, base_runs) =
        decode_extent_runs(&attr, sources.cluster_size, sources.volume_size)?;
    let base_extent = ListEntry {
        type_id: NtfsAttributeType::AttributeList as u32,
        starting_vcn: base_lowest_vcn,
        target_reference: base_reference,
        id: base_attr_id,
    };
    let runs = close_extent_runs(
        base_runs,
        data_size,
        base_extent,
        |runs, prefix_len| {
            let mut entries = Vec::new();
            stream_reader.visit(runs, prefix_len, sources.stop, true, |entry| {
                if entry.type_id == NtfsAttributeType::AttributeList as u32 {
                    entries.push(entry);
                }
            })?;
            Some(entries)
        },
        |entry| {
            if sources.stop.load(Ordering::Relaxed) {
                return None;
            }
            let number = entry.target_record();
            if number == base.number {
                decode_extension_extent(
                    number,
                    base.data,
                    entry,
                    None,
                    sources.cluster_size,
                    sources.volume_size,
                )
            } else {
                match sources.extensions.get(&number) {
                    Some(&slot) => decode_extension_extent(
                        number,
                        sources.arena.get(slot),
                        entry,
                        Some(base_reference),
                        sources.cluster_size,
                        sources.volume_size,
                    ),
                    None => record_reader.read_record(number).and_then(|bytes| {
                        decode_extension_extent(
                            number,
                            bytes,
                            entry,
                            Some(base_reference),
                            sources.cluster_size,
                            sources.volume_size,
                        )
                    }),
                }
            }
        },
    )?;
    Some(AttributeListSource::NonResident { data_size, runs })
}

fn file_name_for_entry(file: &NtfsFile<'_>, id: u16) -> Option<NtfsFileName> {
    let mut found = None;
    file.attributes(|attr| {
        if found.is_none()
            && attr.header.type_id == NtfsAttributeType::FileName as u32
            && attr.header.id == id
        {
            found = attr.as_name();
        }
    });
    found
}

fn resolve_file_name_entry(
    base: &NtfsFile<'_>,
    entry: ListEntry,
    ext: &FxHashMap<u64, u32>,
    arena: &RecordArena,
    rr: &mut LazyRecordReader<'_>,
) -> Option<NtfsFileName> {
    let number = entry.target_record();
    if number == base.number {
        return file_name_for_entry(base, entry.id);
    }
    let base_reference = base.reference_number();
    let pick = |bytes: &[u8]| {
        let target = NtfsFile::new(number, bytes);
        if !file_matches_reference(&target, entry.target_reference)
            || !extension_belongs_to(&target, base_reference)
        {
            return None;
        }
        file_name_for_entry(&target, entry.id)
    };
    match ext.get(&number) {
        Some(&slot) => pick(arena.get(slot)),
        None => rr.read_record(number).and_then(pick),
    }
}

/// Resolve every searchable hard-link name of a record whose `$FILE_NAME`
/// attributes may live in extension records. Resident and non-resident
/// (including split-extent) lists share the checked parser. One unresolved
/// entry fails the whole object closed instead of publishing a partial set.
fn resolve_attr_list_names(
    base: &NtfsFile,
    sources: &ResolveSources<'_>,
    rr: &mut LazyRecordReader,
    stream_reader: &mut LazyStreamReader,
) -> Option<Vec<NtfsFileName>> {
    if sources.stop.load(Ordering::Relaxed) {
        return None;
    }
    let source = load_attribute_list(base, sources, rr, stream_reader)?;
    let mut names = Vec::new();
    let mut seen_entries = FxHashSet::default();
    let mut base_name_ids = FxHashSet::default();
    base.attributes(|attribute| {
        if attribute.header.type_id == NtfsAttributeType::FileName as u32 {
            base_name_ids.insert(attribute.header.id);
        }
    });
    let mut valid = true;
    {
        let mut consider = |entry: ListEntry| {
            if sources.stop.load(Ordering::Relaxed)
                || entry.type_id != NtfsAttributeType::FileName as u32
                || !seen_entries.insert((entry.target_reference, entry.id))
            {
                return;
            }
            let Some(name) =
                resolve_file_name_entry(base, entry, sources.extensions, sources.arena, rr)
            else {
                valid = false;
                return;
            };
            if name.header.name_length == 0 {
                valid = false;
                return;
            }
            let namespace = name.header.namespace;
            if is_searchable_namespace(namespace) {
                names.push(name);
            } else if namespace != NtfsFileNamespace::Dos as u8 {
                valid = false;
            }
        };
        match source {
            AttributeListSource::Resident(list) => {
                for entry in parse_list_entries(list, false)? {
                    consider(entry);
                }
            }
            AttributeListSource::NonResident { data_size, runs } => {
                stream_reader.visit(&runs, data_size, sources.stop, false, &mut consider)?;
            }
        }
    }
    let base_reference = base.reference_number();
    if !base_name_ids
        .iter()
        .all(|id| seen_entries.contains(&(base_reference, *id)))
    {
        valid = false;
    }
    if sources.stop.load(Ordering::Relaxed) || !valid {
        return None;
    }
    let mut unique = FxHashSet::default();
    names.retain(|name| {
        let data = name.data;
        let units = name.header.name_length as usize;
        unique.insert((
            name.header.parent_directory_reference,
            data[..units].to_vec(),
        ))
    });
    (!names.is_empty()).then_some(names)
}

/// Resolve deferred $`ATTRIBUTE_LIST` names in parallel — almost entirely
/// from RAM: every target is an extension record and the whole $MFT just
/// streamed through the pipeline, so `ext` already holds the bytes
/// (ADR-0011). Chunk order is preserved, so `EntryId` assignment matches a
/// serial loop.
pub(super) fn resolve_deferred(
    context: DeferredContext<'_>,
    deferred: &[(u64, Option<u32>)],
) -> Result<Vec<ParsedBatch>, DeferredError> {
    use rayon::prelude::*;
    const DEFER_CHUNK: usize = 256;

    let DeferredContext {
        volume_path,
        runmap,
        record_size,
        cluster_size,
        volume_size,
        extensions,
        arena,
        metadata,
        stop,
    } = context;
    if stop.load(Ordering::Relaxed) {
        return Err(DeferredError::Cancelled);
    }
    let results: Vec<Result<ParsedBatch, DeferredError>> = deferred
        .par_chunks(DEFER_CHUNK)
        .map(|chunk| {
            let mut out = ParsedBatch::default();
            // A spilled base record must stay borrowed while its attribute
            // list resolves extension records. Separate readers prevent an
            // extension read from overwriting the base reader's buffer.
            let mut base_rr = LazyRecordReader::new(volume_path, runmap, record_size);
            let mut extension_rr = LazyRecordReader::new(volume_path, runmap, record_size);
            let mut stream_reader = LazyStreamReader::new(volume_path);
            let sources = ResolveSources {
                extensions,
                arena,
                cluster_size,
                volume_size,
                stop,
            };
            for &(reference, slot) in chunk {
                if stop.load(Ordering::Relaxed) {
                    return Err(DeferredError::Cancelled);
                }
                let number = reference & 0x0000_FFFF_FFFF_FFFF;
                let bytes = match slot {
                    Some(slot) => Some(arena.get(slot)),
                    None => base_rr.read_record(number),
                };
                let Some(bytes) = bytes else {
                    let snapshot = metadata.links(reference);
                    if stop.load(Ordering::Relaxed) {
                        return Err(DeferredError::Cancelled);
                    }
                    if matches!(snapshot, LinkSnapshot::Gone) {
                        continue;
                    }
                    return Err(DeferredError::Incomplete(reference));
                };
                let f = NtfsFile::new(number, bytes);
                if f.reference_number() != reference {
                    return Err(DeferredError::Incomplete(reference));
                }
                let resolved =
                    resolve_attr_list_names(&f, &sources, &mut extension_rr, &mut stream_reader);
                if let Some(names) = resolved {
                    for name in names {
                        out.push_named(&f, &name);
                    }
                } else {
                    if stop.load(Ordering::Relaxed) {
                        return Err(DeferredError::Cancelled);
                    }
                    let snapshot = metadata.links(reference);
                    if stop.load(Ordering::Relaxed) {
                        return Err(DeferredError::Cancelled);
                    }
                    match snapshot {
                        LinkSnapshot::Present(links) if !links.is_empty() => {
                            for link in links {
                                out.push_link(&f, link.parent_frn, &link.name);
                            }
                        }
                        LinkSnapshot::Gone => {}
                        LinkSnapshot::Present(_) | LinkSnapshot::Failed => {
                            return Err(DeferredError::Incomplete(reference));
                        }
                    }
                }
            }
            out.deferred_name_read_failures =
                base_rr.failures + extension_rr.failures + stream_reader.failures;
            Ok(out)
        })
        .collect();
    if stop.load(Ordering::Relaxed) {
        return Err(DeferredError::Cancelled);
    }
    results.into_iter().collect()
}
