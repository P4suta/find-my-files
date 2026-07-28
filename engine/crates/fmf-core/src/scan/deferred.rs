//! Deferred $`ATTRIBUTE_LIST` name resolution (ADR-0011): name-bearing
//! extension records are cached in RAM while the $MFT streams through, so
//! this pass resolves names without disk reads; anything missing (cache
//! cap, torn records) falls back to a targeted read of the live volume.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ondisk::attribute_list::{
    ListEntry, ListStreamError, StreamRun, close_extent_runs, covered_prefix, decode_extent_runs,
    parse_list_entries, visit_list_stream,
};
use crate::ondisk::fixup::apply_fixup;
use crate::ondisk::ntfs::{NtfsAttributeType, NtfsFile, NtfsFileName, NtfsFileNamespace};
use crate::ondisk::record::attributes_complete;

use super::parse::{ParsedBatch, RecordArena, extract_attrs};
use super::volume_io::{RunMap, SectorAlignedReader, open_raw_volume};
use crate::mft::{IncompleteCause, IncompleteObject, is_searchable_namespace};
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
    pub(super) sector_size: usize,
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
    Incomplete(IncompleteObject),
}

/// A chunk's give-up, before the chunk's read-failure tally is attached.
///
/// Separate from [`DeferredError`] on purpose: the tally lives in the chunk's
/// readers, which stay mutably borrowed for as long as the resolution loop
/// holds record bytes. Splitting the loop into its own function ends those
/// borrows at exactly one place — where the counters are read and stamped onto
/// the outcome, success or failure alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkGiveUp {
    /// `None` marks cancellation; no object is at fault.
    object: Option<(u64, IncompleteCause)>,
}

impl ChunkGiveUp {
    const CANCELLED: Self = Self { object: None };

    const fn incomplete(reference: u64, cause: IncompleteCause) -> Self {
        Self {
            object: Some((reference, cause)),
        }
    }

    const fn into_error(self, name_read_failures: u64) -> DeferredError {
        match self.object {
            None => DeferredError::Cancelled,
            Some((reference, cause)) => DeferredError::Incomplete(IncompleteObject {
                reference,
                cause,
                name_read_failures,
            }),
        }
    }
}

/// The three lazily-opened volume readers one deferred chunk shares.
struct ChunkReaders<'a> {
    /// A spilled base record must stay borrowed while its attribute list
    /// resolves extension records, so the two record readers are separate:
    /// an extension read must not overwrite the base reader's buffer.
    base: LazyRecordReader<'a>,
    extension: LazyRecordReader<'a>,
    stream: LazyStreamReader<'a>,
}

impl ChunkReaders<'_> {
    /// Targeted volume reads this chunk failed. Each one is a name that stays
    /// unresolved until the next rescan, so it is reported on every exit
    /// path — a chunk that gives up is precisely the one whose read failures
    /// explain the give-up.
    const fn name_read_failures(&self) -> u64 {
        self.base.failures + self.extension.failures + self.stream.failures
    }
}

#[derive(Clone, Copy)]
struct ResolveSources<'a> {
    extensions: &'a FxHashMap<u64, u32>,
    arena: &'a RecordArena,
    sector_size: usize,
    cluster_size: u64,
    volume_size: u64,
    stop: &'a AtomicBool,
}

/// Random access to single records for the deferred attribute-list pass.
struct RecordReader<'a> {
    file: std::fs::File,
    map: &'a RunMap,
    record_size: usize,
    sector_size: usize,
    buf: Vec<u8>,
}

impl RecordReader<'_> {
    fn read_record(&mut self, number: u64) -> Option<&[u8]> {
        let logical = number.checked_mul(self.record_size as u64)?;
        self.buf.resize(self.record_size, 0);
        self.map
            .read_exact_logical(&mut self.file, logical, &mut self.buf)
            .ok()?;
        if !NtfsFile::is_valid(&self.buf, self.sector_size)
            || !apply_fixup(&mut self.buf, self.sector_size)
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
    sector_size: usize,
    inner: Option<RecordReader<'a>>,
    failed: bool,
    /// Failed `read_record` calls — each one is a name that stays
    /// unresolved until the next rescan. `resolve_deferred` folds this into
    /// its batch, so the count reaches `ScanStats` (don't go silent).
    failures: u64,
}

impl<'a> LazyRecordReader<'a> {
    const fn new(
        volume_path: &'a str,
        map: &'a RunMap,
        record_size: usize,
        sector_size: usize,
    ) -> Self {
        LazyRecordReader {
            volume_path,
            map,
            record_size,
            sector_size,
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
                        sector_size: self.sector_size,
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
    /// The volume's logical sector size — the granularity every read against
    /// the raw handle must respect. See [`SectorAlignedReader`].
    sector_size: usize,
    inner: Option<std::fs::File>,
    failed: bool,
    failures: u64,
}

impl<'a> LazyStreamReader<'a> {
    const fn new(volume_path: &'a str, sector_size: usize) -> Self {
        Self {
            volume_path,
            sector_size,
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
        let sector_size = self.sector_size;
        let Some(file) = self.inner.as_mut() else {
            self.failures += 1;
            return None;
        };
        let mut aligned = SectorAlignedReader::new(file, sector_size);
        match visit_list_stream(&mut aligned, runs, data_size, stop, prefix, visit) {
            Ok(()) => Some(()),
            Err(ListStreamError::Io(error)) => {
                self.failures += 1;
                tracing::warn!(
                    os = error.raw_os_error().unwrap_or(-1),
                    data_size,
                    runs = runs.len(),
                    "non-resident $ATTRIBUTE_LIST stream read failed"
                );
                None
            }
            Err(ListStreamError::Invalid | ListStreamError::Cancelled) => None,
        }
    }
}

const fn file_matches_reference(file: &NtfsFile<'_>, expected: u64) -> bool {
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
    sector_size: usize,
    cluster_size: u64,
    volume_size: u64,
) -> Option<Vec<StreamRun>> {
    let file = NtfsFile::parse(number, bytes, sector_size)?;
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

/// Why the $MFT alone could not yield an object's complete name set.
///
/// Not an error on its own: every arm falls back to the live link query, which
/// is authoritative. It is recorded so that when the live query *also* fails —
/// the one case where the object is genuinely lost — the report can say which
/// half broke. Losing that was what made this path unexplainable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameGiveUp {
    Cancelled,
    NoAttributeList,
    ResidentBodyMissing,
    NonResidentHeaderUnusable,
    ExtentRunsUndecodable,
    ExtentClosureIncomplete,
    ListEntriesInvalid,
    EntryUnresolvable,
    EmptyName,
    UnsearchableNamespace,
    BaseNameNotListed,
}

impl NameGiveUp {
    /// A `snake_case`-free tag: the diagnostic sink only passes values made of
    /// ASCII alphanumerics and `-_.:`.
    const fn as_tag(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::NoAttributeList => "no-attribute-list",
            Self::ResidentBodyMissing => "resident-body-missing",
            Self::NonResidentHeaderUnusable => "nonresident-header-unusable",
            Self::ExtentRunsUndecodable => "extent-runs-undecodable",
            Self::ExtentClosureIncomplete => "extent-closure-incomplete",
            Self::ListEntriesInvalid => "list-entries-invalid",
            Self::EntryUnresolvable => "entry-unresolvable",
            Self::EmptyName => "empty-name",
            Self::UnsearchableNamespace => "unsearchable-namespace",
            Self::BaseNameNotListed => "base-name-not-listed",
        }
    }
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
) -> Result<AttributeListSource<'a>, NameGiveUp> {
    let attr = base
        .get_attribute(NtfsAttributeType::AttributeList)
        .ok_or(NameGiveUp::NoAttributeList)?;
    if attr.header.is_non_resident == 0 {
        return attr
            .get_resident()
            .map(AttributeListSource::Resident)
            .ok_or(NameGiveUp::ResidentBodyMissing);
    }

    let base_reference = base.reference_number();
    let base_attr_id = attr.header.id;
    let base_lowest_vcn = attr
        .nonresident_header()
        .and_then(|header| u64::try_from(header.lowest_vcn).ok())
        .ok_or(NameGiveUp::NonResidentHeaderUnusable)?;
    let Some((data_size, base_runs)) =
        decode_extent_runs(&attr, sources.cluster_size, sources.volume_size)
    else {
        return Err(NameGiveUp::ExtentRunsUndecodable);
    };
    let base_extent = ListEntry::unnamed(
        NtfsAttributeType::AttributeList as u32,
        base_lowest_vcn,
        base_reference,
        base_attr_id,
    );
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
                    sources.sector_size,
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
                        sources.sector_size,
                        sources.cluster_size,
                        sources.volume_size,
                    ),
                    None => record_reader.read_record(number).and_then(|bytes| {
                        decode_extension_extent(
                            number,
                            bytes,
                            entry,
                            Some(base_reference),
                            sources.sector_size,
                            sources.cluster_size,
                            sources.volume_size,
                        )
                    }),
                }
            }
        },
    )
    .ok_or(NameGiveUp::ExtentClosureIncomplete)?;
    Ok(AttributeListSource::NonResident { data_size, runs })
}

#[derive(Clone)]
struct ResolvedFileName {
    parent_reference: u64,
    namespace: u8,
    utf16le: Vec<u8>,
}

impl From<NtfsFileName<'_>> for ResolvedFileName {
    fn from(name: NtfsFileName<'_>) -> Self {
        Self {
            parent_reference: name.header.parent_directory_reference,
            namespace: name.header.namespace,
            utf16le: name.utf16le.to_vec(),
        }
    }
}

fn file_name_for_entry(file: &NtfsFile<'_>, entry: ListEntry) -> Option<ResolvedFileName> {
    let mut found = None;
    file.attributes(|attr| {
        if found.is_none()
            && attr.header.type_id == NtfsAttributeType::FileName as u32
            && attr.header.id == entry.id
            && attr.header.name_length == entry.name_length
        {
            found = attr.as_name().map(ResolvedFileName::from);
        }
    });
    found
}

fn resolve_file_name_entry(
    base: &NtfsFile<'_>,
    entry: ListEntry,
    ext: &FxHashMap<u64, u32>,
    arena: &RecordArena,
    sector_size: usize,
    rr: &mut LazyRecordReader<'_>,
) -> Option<ResolvedFileName> {
    let number = entry.target_record();
    if number == base.number {
        return file_name_for_entry(base, entry);
    }
    let base_reference = base.reference_number();
    let pick = |bytes: &[u8]| {
        let target = NtfsFile::parse(number, bytes, sector_size)?;
        if !file_matches_reference(&target, entry.target_reference)
            || !extension_belongs_to(&target, base_reference)
        {
            return None;
        }
        file_name_for_entry(&target, entry)
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
) -> Result<Vec<ResolvedFileName>, NameGiveUp> {
    if sources.stop.load(Ordering::Relaxed) {
        return Err(NameGiveUp::Cancelled);
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
    let mut give_up = None;
    {
        let mut consider = |entry: ListEntry| {
            if sources.stop.load(Ordering::Relaxed)
                || entry.type_id != NtfsAttributeType::FileName as u32
                || !seen_entries.insert((entry.target_reference, entry.id))
            {
                return;
            }
            let Some(name) = resolve_file_name_entry(
                base,
                entry,
                sources.extensions,
                sources.arena,
                sources.sector_size,
                rr,
            ) else {
                give_up = Some(NameGiveUp::EntryUnresolvable);
                return;
            };
            if name.utf16le.is_empty() {
                give_up = Some(NameGiveUp::EmptyName);
                return;
            }
            let namespace = name.namespace;
            if is_searchable_namespace(namespace) {
                names.push(name);
            } else if namespace != NtfsFileNamespace::Dos as u8 {
                give_up = Some(NameGiveUp::UnsearchableNamespace);
            }
        };
        match source {
            AttributeListSource::Resident(list) => {
                for entry in
                    parse_list_entries(list, false).ok_or(NameGiveUp::ListEntriesInvalid)?
                {
                    consider(entry);
                }
            }
            AttributeListSource::NonResident { data_size, runs } => {
                stream_reader
                    .visit(&runs, data_size, sources.stop, false, &mut consider)
                    .ok_or(NameGiveUp::ListEntriesInvalid)?;
            }
        }
    }
    let base_reference = base.reference_number();
    if !base_name_ids
        .iter()
        .all(|id| seen_entries.contains(&(base_reference, *id)))
    {
        give_up = Some(NameGiveUp::BaseNameNotListed);
    }
    if sources.stop.load(Ordering::Relaxed) {
        return Err(NameGiveUp::Cancelled);
    }
    if let Some(reason) = give_up {
        return Err(reason);
    }
    let mut unique = FxHashSet::default();
    if names
        .iter()
        .any(|name| !unique.insert((name.parent_reference, name.utf16le.clone())))
    {
        return Err(NameGiveUp::EntryUnresolvable);
    }
    if names.is_empty() {
        return Err(NameGiveUp::EmptyName);
    }
    Ok(names)
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
        sector_size,
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
            let mut readers = ChunkReaders {
                base: LazyRecordReader::new(volume_path, runmap, record_size, sector_size),
                extension: LazyRecordReader::new(volume_path, runmap, record_size, sector_size),
                stream: LazyStreamReader::new(volume_path, sector_size),
            };
            let sources = ResolveSources {
                extensions,
                arena,
                sector_size,
                cluster_size,
                volume_size,
                stop,
            };
            let outcome = resolve_chunk(chunk, &sources, metadata, &mut readers, &mut out);
            // One read of the counters, after every borrow the loop held has
            // ended, on both exit paths.
            let name_read_failures = readers.name_read_failures();
            match outcome {
                Ok(()) => {
                    out.deferred_name_read_failures = name_read_failures;
                    Ok(out)
                }
                Err(give_up) => Err(give_up.into_error(name_read_failures)),
            }
        })
        .collect();
    if stop.load(Ordering::Relaxed) {
        return Err(DeferredError::Cancelled);
    }
    results.into_iter().collect()
}

/// Resolve one chunk's objects into `out`. Split out of `resolve_deferred` so
/// that its borrows of `readers` end before the caller reads their failure
/// counters — see [`ChunkGiveUp`].
fn resolve_chunk(
    chunk: &[(u64, Option<u32>)],
    sources: &ResolveSources<'_>,
    metadata: &MetadataSource,
    readers: &mut ChunkReaders<'_>,
    out: &mut ParsedBatch,
) -> Result<(), ChunkGiveUp> {
    let stop = sources.stop;
    let sector_size = sources.sector_size;
    for &(reference, slot) in chunk {
        if stop.load(Ordering::Relaxed) {
            return Err(ChunkGiveUp::CANCELLED);
        }
        let number = reference & 0x0000_FFFF_FFFF_FFFF;
        let bytes = match slot {
            Some(slot) => Some(sources.arena.get(slot)),
            None => readers.base.read_record(number),
        };
        let Some(bytes) = bytes else {
            let snapshot = metadata.links(reference);
            if stop.load(Ordering::Relaxed) {
                return Err(ChunkGiveUp::CANCELLED);
            }
            if matches!(snapshot, LinkSnapshot::Gone) {
                continue;
            }
            return Err(ChunkGiveUp::incomplete(
                reference,
                IncompleteCause::RecordUnreadable,
            ));
        };
        let Some(f) = NtfsFile::parse(number, bytes, sector_size) else {
            return Err(ChunkGiveUp::incomplete(
                reference,
                IncompleteCause::MalformedRecord,
            ));
        };
        if f.reference_number() != reference {
            return Err(ChunkGiveUp::incomplete(
                reference,
                IncompleteCause::ReferenceMismatch,
            ));
        }
        let Some(attrs) = extract_attrs(&f) else {
            return Err(ChunkGiveUp::incomplete(
                reference,
                IncompleteCause::AttributesMissing,
            ));
        };
        // The live source is the authoritative size/mtime for a spilled $DATA,
        // but "no size" is a property of one object and must never decide the
        // fate of the volume. `OpenFileById` cannot open NTFS metadata files,
        // and `\$Extend\$ObjId`/`$Quota`/`$Reparse` all live past
        // `FIRST_NORMAL_RECORD` *and* carry an $ATTRIBUTE_LIST, so every real
        // volume reaches this line with an unanswerable object. A file deleted
        // between the $MFT snapshot and this query is unanswerable too.
        //
        // `parse.rs` already makes this trade for the non-deferred path:
        // publish the record with the size it can prove (0) rather than refuse
        // the index. The base record's $STANDARD_INFORMATION is present either
        // way, so only the size degrades — the mtime stays authoritative.
        // Counted so a flood, which is a real problem, stays visible.
        let attrs = if let Some((size, mtime)) = metadata.stat(reference) {
            attrs.with_stat(size, mtime)
        } else {
            out.deferred_stat_failures += 1;
            attrs
        };
        let resolved =
            resolve_attr_list_names(&f, sources, &mut readers.extension, &mut readers.stream);
        if let Ok(names) = resolved {
            for name in names {
                if !out.push_utf16le_link_with_attrs(
                    &f,
                    name.parent_reference,
                    &name.utf16le,
                    attrs,
                ) {
                    return Err(ChunkGiveUp::incomplete(
                        reference,
                        IncompleteCause::LinkRejected,
                    ));
                }
            }
        } else {
            if stop.load(Ordering::Relaxed) {
                return Err(ChunkGiveUp::CANCELLED);
            }
            let snapshot = metadata.links(reference);
            if stop.load(Ordering::Relaxed) {
                return Err(ChunkGiveUp::CANCELLED);
            }
            match snapshot {
                LinkSnapshot::Present(links) if !links.is_empty() => {
                    for link in links {
                        if !out.push_link(&f, link.parent_frn, &link.name, attrs) {
                            return Err(ChunkGiveUp::incomplete(
                                reference,
                                IncompleteCause::LinkRejected,
                            ));
                        }
                    }
                }
                LinkSnapshot::Gone => {}
                LinkSnapshot::Present(_) | LinkSnapshot::Failed => {
                    // Both halves failed, so this object really is lost — the
                    // one moment the $MFT-side reason is worth reporting. On a
                    // normal fallback it is noise: the live query is
                    // authoritative and answering it is the expected outcome.
                    tracing::warn!(
                        reason = resolved.err().unwrap_or(NameGiveUp::Cancelled).as_tag(),
                        reference,
                        "no name source could complete this object"
                    );
                    return Err(ChunkGiveUp::incomplete(
                        reference,
                        IncompleteCause::LinkSetUnavailable,
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure tally used to be summed after the resolution loop and
    /// therefore only on the loop's *success* path — it was discarded exactly
    /// when it was needed. A give-up must carry both the reason it gave up on
    /// and the volume reads that failed getting there.
    #[test]
    fn a_give_up_reports_its_cause_and_the_reads_that_failed() {
        let reference = (1u64 << 48) | 0x4D;
        let runmap = super::super::volume_io::RunMap { runs: Vec::new() };
        let arena = RecordArena::new(1024);
        let extensions = FxHashMap::default();
        let stop = Arc::new(AtomicBool::new(false));
        // Neither an arena slot nor an openable volume, so the targeted base
        // record read is attempted and fails.
        let context = DeferredContext {
            volume_path: "no-such-volume-for-this-fixture",
            runmap: &runmap,
            record_size: 1024,
            sector_size: 512,
            cluster_size: 4096,
            volume_size: 1 << 20,
            extensions: &extensions,
            arena: &arena,
            metadata: &MetadataSource::none(),
            stop: &stop,
        };

        let Err(error) = resolve_deferred(context, &[(reference, None)]) else {
            panic!("an unreadable base record cannot resolve");
        };

        let DeferredError::Incomplete(object) = error else {
            panic!("an unreadable base record is not a cancellation: {error:?}");
        };
        assert_eq!(object.reference, reference);
        assert_eq!(object.cause, IncompleteCause::RecordUnreadable);
        assert_eq!(
            object.name_read_failures, 1,
            "the failed volume read survives the give-up"
        );
    }
}
