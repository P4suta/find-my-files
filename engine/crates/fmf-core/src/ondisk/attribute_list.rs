//! Checked primitives shared by initial-scan and live-USN `$ATTRIBUTE_LIST`
//! resolution.
//!
//! This module owns the untrusted byte grammar and non-resident run
//! arithmetic; callers own record acquisition and name policy.

#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use rustc_hash::{FxHashMap, FxHashSet};

use super::ntfs::{
    ATTRIBUTE_LIST_ENTRY_BYTES, NONRESIDENT_HEADER_BYTES, NtfsAttribute, NtfsAttributeType,
};

const FILE_REFERENCE_RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const STREAM_BUFFER_BYTES: usize = 64 << 10;
const MAX_ATTRIBUTE_TYPE: u32 = 0x100;
// One file is limited to 1,024 hard links on supported Windows releases. This
// deliberately generous ceiling leaves two orders of magnitude for named
// streams and split extents while bounding LocalSystem memory on corrupt media.
const MAX_LIST_ENTRIES: usize = 262_144;
// Every `$FILE_NAME` entry costs one target-record resolution, so it is
// budgeted an order of magnitude tighter than the whole list. Windows caps a
// file at 1,024 hard links and a link contributes at most two attributes (a
// Win32 name plus its separate DOS pair), so 4,096 is double the largest shape
// a real volume can present while keeping crafted fan-out bounded.
const MAX_FILE_NAME_ENTRIES: usize = 4_096;
const MAX_LIST_BYTES: u64 = 16 << 20;
const MAX_LIST_RUNS: usize = 16_384;
const MAX_CLOSURE_PASSES: usize = 64;
const MAX_CLOSURE_SCAN_BYTES: u64 = 64 << 20;

/// One decoded `$ATTRIBUTE_LIST` entry: where an attribute of a split file
/// actually lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListEntry {
    /// Attribute type code of the referenced attribute.
    pub type_id: u32,
    /// UTF-16 code-unit count of the attribute's optional stream name.
    ///
    /// The scanner only follows `$ATTRIBUTE_LIST` and `$FILE_NAME` entries,
    /// both of which are required to be unnamed. Keeping the wire length here
    /// lets target-record matching prove that invariant instead of silently
    /// dropping the metadata.
    pub name_length: u8,
    /// First virtual cluster the referenced extent covers.
    pub starting_vcn: u64,
    /// FRN of the record holding the referenced attribute.
    pub target_reference: u64,
    /// Instance id of the referenced attribute within its target record.
    pub id: u16,
}

impl ListEntry {
    /// Build an entry for an attribute with no stream name.
    #[must_use]
    pub const fn unnamed(type_id: u32, starting_vcn: u64, target_reference: u64, id: u16) -> Self {
        Self {
            type_id,
            name_length: 0,
            starting_vcn,
            target_reference,
            id,
        }
    }

    /// The target FRN with its sequence value masked off, i.e. the plain
    /// $MFT record number.
    #[must_use]
    pub const fn target_record(self) -> u64 {
        self.target_reference & FILE_REFERENCE_RECORD_MASK
    }
}

#[derive(Debug)]
struct DecodedListEntry {
    public: ListEntry,
    name: Box<[u16]>,
}

#[derive(Default)]
struct EntrySequence {
    previous_type: Option<u32>,
    last_vcn_by_name: FxHashMap<(u32, Box<[u16]>), u64>,
    targets: FxHashSet<(u64, u16)>,
    count: usize,
    file_names: usize,
}

impl EntrySequence {
    fn accept(&mut self, decoded: &DecodedListEntry) -> bool {
        // Cross-name ordering depends on the volume's authoritative `$UpCase`
        // table, which this byte grammar intentionally does not guess. Type
        // order and VCN order within an exact raw name remain independently
        // provable; the map also rejects an exact (type, name, VCN) duplicate
        // even if corrupt input interleaves another name between its entries.
        let Some(count) = self.count.checked_add(1) else {
            return false;
        };
        self.count = count;
        if self.count > MAX_LIST_ENTRIES
            || self
                .previous_type
                .is_some_and(|previous| previous > decoded.public.type_id)
            || !self
                .targets
                .insert((decoded.public.target_reference, decoded.public.id))
        {
            return false;
        }
        self.previous_type = Some(decoded.public.type_id);
        if decoded.public.type_id == NtfsAttributeType::FileName as u32 {
            // `$FILE_NAME` is resident-only and `validate_entry_header` already
            // pins its VCN to zero, so it can never be a split extent and the
            // monotonic-VCN rule below cannot discriminate between its entries.
            // Applying it anyway rejected the two shapes every real volume
            // carries: a file with several hard links, and a long name paired
            // with its 8.3 DOS name. Repeats and cycles are instead carried by
            // the `(target_reference, id)` set above, which is the key the name
            // resolver dereferences, plus this explicit fan-out budget.
            let Some(file_names) = self.file_names.checked_add(1) else {
                return false;
            };
            self.file_names = file_names;
            return file_names <= MAX_FILE_NAME_ENTRIES;
        }
        let key = (decoded.public.type_id, decoded.name.clone());
        match self.last_vcn_by_name.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut previous) => {
                if *previous.get() >= decoded.public.starting_vcn {
                    return false;
                }
                previous.insert(decoded.public.starting_vcn);
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(decoded.public.starting_vcn);
            }
        }
        true
    }
}

/// One decoded extent of a non-resident stream, in absolute bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRun {
    /// Byte offset within the stream.
    pub logical: u64,
    /// Byte offset on the volume, or `None` for a sparse run.
    pub physical: Option<u64>,
    /// Length of the run in bytes.
    pub len: u64,
}

fn physical_ranges_are_disjoint(runs: &[StreamRun]) -> bool {
    let mut ranges = Vec::with_capacity(runs.len());
    for run in runs {
        let Some(physical) = run.physical else {
            return false;
        };
        let Some(end) = physical.checked_add(run.len) else {
            return false;
        };
        if run.len == 0 {
            return false;
        }
        ranges.push((physical, end));
    }
    ranges.sort_unstable();
    !ranges.windows(2).any(|pair| pair[0].1 > pair[1].0)
}

fn partial_runs_are_valid(runs: &[StreamRun], data_size: u64) -> bool {
    if data_size == 0
        || data_size > MAX_LIST_BYTES
        || runs.is_empty()
        || runs.len() > MAX_LIST_RUNS
        || !physical_ranges_are_disjoint(runs)
    {
        return false;
    }
    let mut ordered = runs.to_vec();
    ordered.sort_unstable_by_key(|run| run.logical);
    if ordered[0].logical != 0 {
        return false;
    }
    let mut previous_end = 0u64;
    for run in ordered {
        let Some(end) = run.logical.checked_add(run.len) else {
            return false;
        };
        // A run may legitimately start at or past `data_size`. Mapping pairs
        // describe the ALLOCATED extent, and NTFS allocates whole clusters and
        // does not shrink the allocation when a stream shrinks — a 96-byte list
        // holding two clusters is ordinary. Those tail runs are never read;
        // `RunReader` bounds itself by `data_size`. Rejecting them here failed
        // the whole volume on real NTFS.
        if run.logical < previous_end {
            return false;
        }
        previous_end = end;
    }
    true
}

fn complete_runs_are_valid(runs: &[StreamRun], data_size: u64) -> bool {
    if !partial_runs_are_valid(runs, data_size) {
        return false;
    }
    let mut ordered = runs.to_vec();
    ordered.sort_unstable_by_key(|run| run.logical);
    let mut logical = 0u64;
    for run in ordered {
        if run.logical != logical {
            return false;
        }
        let Some(end) = logical.checked_add(run.len) else {
            return false;
        };
        logical = end;
    }
    // Contiguous from zero (checked above) and covering the whole stream. Runs
    // that continue past `data_size` are the allocated tail, not corruption.
    logical >= data_size
}

fn le_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

fn le_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn le_u64(data: &[u8], off: usize) -> u64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&data[off..off + 8]);
    u64::from_le_bytes(out)
}

fn declared_entry_length(header: &[u8]) -> Option<usize> {
    let len = usize::from(le_u16(header, 4));
    (len >= ATTRIBUTE_LIST_ENTRY_BYTES && len.is_multiple_of(8)).then_some(len)
}

fn validate_entry_header(header: &[u8], len: usize) -> Option<(u32, usize, usize, u64)> {
    let type_id = le_u32(header, 0);
    if type_id == 0
        || type_id > MAX_ATTRIBUTE_TYPE
        || type_id == NtfsAttributeType::End as u32
        || !type_id.is_multiple_of(0x10)
    {
        return None;
    }
    let name_len = usize::from(header[6]);
    let name_off = usize::from(header[7]);
    let name_bytes = name_len.checked_mul(size_of::<u16>())?;
    if name_len > 0
        && (name_off != ATTRIBUTE_LIST_ENTRY_BYTES
            || name_off.checked_add(name_bytes).is_none_or(|end| end > len))
    {
        return None;
    }
    let starting_vcn = le_u64(header, 8);
    let target_reference = le_u64(header, 16);
    if matches!(
        type_id,
        id if id == NtfsAttributeType::AttributeList as u32
            || id == NtfsAttributeType::FileName as u32
    ) && name_len != 0
        || type_id == NtfsAttributeType::FileName as u32 && starting_vcn != 0
    {
        return None;
    }
    (target_reference >> 48 != 0).then_some((type_id, name_len, name_off, starting_vcn))
}

fn decode_list_entry(data: &[u8]) -> Option<DecodedListEntry> {
    let header = data.get(..ATTRIBUTE_LIST_ENTRY_BYTES)?;
    let len = declared_entry_length(header)?;
    if len != data.len() {
        return None;
    }
    let (type_id, name_len, name_off, starting_vcn) = validate_entry_header(header, len)?;
    let name = if name_len == 0 {
        Box::default()
    } else {
        let name_end = name_off.checked_add(name_len.checked_mul(size_of::<u16>())?)?;
        data.get(name_off..name_end)?
            .chunks_exact(size_of::<u16>())
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
            .collect::<Vec<_>>()
            .into_boxed_slice()
    };
    Some(DecodedListEntry {
        public: ListEntry {
            type_id,
            name_length: u8::try_from(name_len).ok()?,
            starting_vcn,
            target_reference: le_u64(header, 16),
            id: le_u16(header, 24),
        },
        name,
    })
}

/// Parse owned entry fields without forming a reference to packed,
/// potentially unaligned disk bytes.
///
/// `prefix` permits only an incomplete last entry (needed while discovering a
/// non-resident list's continuation extents); a complete stream fails closed
/// on the same bytes.
#[must_use]
pub fn parse_list_entries(data: &[u8], prefix: bool) -> Option<Vec<ListEntry>> {
    if u64::try_from(data.len()).ok()? > MAX_LIST_BYTES {
        return None;
    }
    let header_len = ATTRIBUTE_LIST_ENTRY_BYTES;
    let mut entries = Vec::new();
    let mut sequence = EntrySequence::default();
    let mut off = 0usize;
    while off < data.len() {
        let remaining = &data[off..];
        if remaining.len() < header_len {
            return prefix.then_some(entries);
        }
        let len = declared_entry_length(remaining)?;
        validate_entry_header(remaining, len)?;
        let entry_end = off.checked_add(len)?;
        if entry_end > data.len() {
            return prefix.then_some(entries);
        }
        let decoded = decode_list_entry(data.get(off..entry_end)?)?;
        if !sequence.accept(&decoded) {
            return None;
        }
        entries.push(decoded.public);
        off = entry_end;
    }
    (prefix || !entries.is_empty()).then_some(entries)
}

fn unsigned_le(data: &[u8]) -> u64 {
    let mut out = [0u8; 8];
    out[..data.len()].copy_from_slice(data);
    u64::from_le_bytes(out)
}

fn signed_le(data: &[u8]) -> i64 {
    let fill = if data.last().is_some_and(|byte| byte & 0x80 != 0) {
        0xFF
    } else {
        0
    };
    let mut out = [fill; 8];
    out[..data.len()].copy_from_slice(data);
    i64::from_le_bytes(out)
}

/// Decode one non-resident extent's mapping pairs into absolute byte ranges.
///
/// All logical/physical arithmetic and volume bounds are checked before I/O.
#[must_use]
pub fn decode_extent_runs(
    attr: &NtfsAttribute<'_>,
    cluster_size: u64,
    volume_size: u64,
) -> Option<(u64, Vec<StreamRun>)> {
    if cluster_size == 0
        || volume_size == 0
        || !cluster_size.is_power_of_two()
        || attr.header.name_length != 0
        || attr.header.flags != 0
        || !matches!(
            attr.header.type_id,
            id if id == NtfsAttributeType::AttributeList as u32
                || id == NtfsAttributeType::Data as u32
        )
        || !attr.len().is_multiple_of(8)
    {
        return None;
    }
    let header = attr.nonresident_header()?;
    let lowest_vcn = u64::try_from(header.lowest_vcn).ok()?;
    let highest_vcn = u64::try_from(header.highest_vcn).ok()?;
    let extent_clusters = highest_vcn.checked_sub(lowest_vcn)?.checked_add(1)?;
    let logical_base = lowest_vcn.checked_mul(cluster_size)?;
    let logical_extent_end = highest_vcn.checked_add(1)?.checked_mul(cluster_size)?;
    if logical_extent_end > volume_size {
        return None;
    }
    let data_size = header.data_size;
    let bytes = attr.data();
    // The six bytes after MappingPairsOffset are reserved for an
    // uncompressed/non-sparse attribute. Flags above deliberately reject the
    // compressed, encrypted, and sparse variants because neither `$MFT::$DATA`
    // nor `$ATTRIBUTE_LIST` may use those encodings in this scanner.
    if bytes.get(34..40)?.iter().any(|&byte| byte != 0) {
        return None;
    }
    if lowest_vcn == 0 {
        let allocated_size = le_u64(bytes, 40);
        let initialized_size = le_u64(bytes, 56);
        if allocated_size == 0
            || data_size == 0
            || attr.header.type_id == NtfsAttributeType::AttributeList as u32
                && data_size > MAX_LIST_BYTES
            || !allocated_size.is_multiple_of(cluster_size)
            || allocated_size > volume_size
            || data_size > allocated_size
            || initialized_size > data_size
            || logical_extent_end > allocated_size
        {
            return None;
        }
    }
    let runs_start = header.data_runs_offset as usize;
    if runs_start != NONRESIDENT_HEADER_BYTES || runs_start >= attr.data().len() {
        return None;
    }
    let mapping = &attr.data()[runs_start..];
    let mut cursor = 0usize;
    let mut previous_lcn = 0i128;
    let mut logical_in_extent = 0u64;
    let mut runs = Vec::new();
    loop {
        let descriptor = *mapping.get(cursor)?;
        cursor += 1;
        if descriptor == 0 {
            break;
        }
        let count_bytes = (descriptor & 0x0F) as usize;
        let offset_bytes = (descriptor >> 4) as usize;
        if count_bytes == 0 || count_bytes > 8 || offset_bytes > 8 {
            return None;
        }
        let count_end = cursor.checked_add(count_bytes)?;
        let count = unsigned_le(mapping.get(cursor..count_end)?);
        cursor = count_end;
        if count == 0 {
            return None;
        }
        let run_len = count.checked_mul(cluster_size)?;
        let logical = logical_base.checked_add(logical_in_extent)?;
        logical_in_extent = logical_in_extent.checked_add(run_len)?;
        // A zero LCN-width is the sparse-run encoding. The supported system
        // streams are required to be ordinary allocated streams, so a sparse
        // mapping is corruption rather than a recoverable hole.
        if offset_bytes == 0 {
            return None;
        }
        let offset_end = cursor.checked_add(offset_bytes)?;
        let delta = i128::from(signed_le(mapping.get(cursor..offset_end)?));
        cursor = offset_end;
        previous_lcn = previous_lcn.checked_add(delta)?;
        let lcn = u64::try_from(previous_lcn).ok()?;
        if lcn == 0 {
            return None;
        }
        let physical = lcn.checked_mul(cluster_size)?;
        if physical.checked_add(run_len)? > volume_size {
            return None;
        }
        runs.push(StreamRun {
            logical,
            physical: Some(physical),
            len: run_len,
        });
    }
    if logical_in_extent != extent_clusters.checked_mul(cluster_size)? {
        return None;
    }
    physical_ranges_are_disjoint(&runs).then_some((data_size, runs))
}

/// Length of the contiguous run coverage starting at logical offset 0.
///
/// Continuation discovery reads only this prefix: bytes past the first gap
/// are not yet proven readable.
#[must_use]
pub fn covered_prefix(runs: &[StreamRun]) -> u64 {
    let mut ordered = runs.to_vec();
    ordered.sort_unstable_by_key(|run| run.logical);
    let mut cursor = 0u64;
    for run in ordered {
        if run.logical != cursor {
            break;
        }
        let Some(next) = cursor.checked_add(run.len) else {
            break;
        };
        cursor = next;
    }
    cursor
}

/// Discover and decode every continuation extent needed to cover a logical
/// stream.
///
/// A newly readable prefix can itself reveal another continuation, so closure
/// is iterative; duplicate descriptors are decoded once.
///
/// Returning `Some` means the runs cover `data_size` contiguously. Any decode
/// failure or an iteration that cannot extend the prefix fails closed.
pub fn close_extent_runs(
    mut runs: Vec<StreamRun>,
    data_size: u64,
    base_extent: ListEntry,
    mut discover: impl FnMut(&[StreamRun], u64) -> Option<Vec<ListEntry>>,
    mut decode: impl FnMut(ListEntry) -> Option<Vec<StreamRun>>,
) -> Option<Vec<StreamRun>> {
    if base_extent.type_id != NtfsAttributeType::AttributeList as u32
        || base_extent.name_length != 0
        || base_extent.target_reference >> 48 == 0
        || !partial_runs_are_valid(&runs, data_size)
    {
        return None;
    }
    let mut seen = FxHashMap::default();
    seen.insert(
        (base_extent.target_reference, base_extent.id),
        (
            base_extent.type_id,
            base_extent.name_length,
            base_extent.starting_vcn,
        ),
    );
    let mut scanned_prefix = 0u64;
    let mut scanned_bytes = 0u64;
    let mut passes = 0usize;
    loop {
        let prefix_len = covered_prefix(&runs).min(data_size);
        if prefix_len == data_size {
            break;
        }
        if prefix_len <= scanned_prefix {
            return None;
        }
        passes += 1;
        scanned_bytes = scanned_bytes.checked_add(prefix_len)?;
        if passes > MAX_CLOSURE_PASSES || scanned_bytes > MAX_CLOSURE_SCAN_BYTES {
            return None;
        }
        let entries = discover(&runs, prefix_len)?;
        if entries.len() > MAX_LIST_ENTRIES {
            return None;
        }
        let mut added_extent = false;
        for entry in entries {
            if entry.type_id != NtfsAttributeType::AttributeList as u32 || entry.name_length != 0 {
                return None;
            }
            if entry.target_reference >> 48 == 0 {
                return None;
            }
            let identity = (entry.type_id, entry.name_length, entry.starting_vcn);
            let target = (entry.target_reference, entry.id);
            if seen.len() == MAX_LIST_ENTRIES && !seen.contains_key(&target) {
                return None;
            }
            match seen.entry(target) {
                std::collections::hash_map::Entry::Occupied(existing) => {
                    if *existing.get() != identity {
                        return None;
                    }
                    continue;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(identity);
                }
            }
            let decoded = decode(entry)?;
            if decoded.is_empty() {
                return None;
            }
            runs.extend(decoded);
            if !partial_runs_are_valid(&runs, data_size) {
                return None;
            }
            added_extent = true;
        }
        scanned_prefix = prefix_len;
        if !added_extent {
            return None;
        }
    }
    complete_runs_are_valid(&runs, data_size).then_some(runs)
}

/// Why streaming a non-resident `$ATTRIBUTE_LIST` stopped.
#[derive(Debug)]
pub enum ListStreamError {
    /// The underlying reader failed. Carries the cause: a raw-volume read can
    /// be refused for reasons that are not I/O faults at all, and collapsing
    /// them into a bare marker is what made this path unexplainable.
    Io(std::io::Error),
    /// The bytes do not form a well-formed entry sequence.
    Invalid,
    /// The caller's stop flag was set.
    Cancelled,
}

impl From<std::io::Error> for ListStreamError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

struct RunReader<'a, R> {
    reader: &'a mut R,
    runs: Vec<StreamRun>,
    run_index: usize,
    position: u64,
    data_size: u64,
}

impl<'a, R: Read + Seek> RunReader<'a, R> {
    fn new(reader: &'a mut R, runs: &[StreamRun], data_size: u64) -> Result<Self, ListStreamError> {
        if !complete_runs_are_valid(runs, data_size) {
            return Err(ListStreamError::Invalid);
        }
        let mut runs = runs.to_vec();
        runs.sort_unstable_by_key(|run| run.logical);
        Ok(Self {
            reader,
            runs,
            run_index: 0,
            position: 0,
            data_size,
        })
    }

    const fn remaining(&self) -> u64 {
        self.data_size - self.position
    }

    fn read_exact(&mut self, mut out: &mut [u8]) -> Result<(), ListStreamError> {
        let out_len = u64::try_from(out.len()).map_err(|_| ListStreamError::Invalid)?;
        if out_len > self.remaining() {
            return Err(ListStreamError::Invalid);
        }
        while !out.is_empty() {
            let run = self
                .runs
                .get(self.run_index)
                .ok_or(ListStreamError::Invalid)?;
            let run_offset = self
                .position
                .checked_sub(run.logical)
                .ok_or(ListStreamError::Invalid)?;
            if run_offset >= run.len {
                self.run_index += 1;
                continue;
            }
            let available = usize::try_from((run.len - run_offset).min(out.len() as u64))
                .map_err(|_| ListStreamError::Invalid)?;
            let (head, tail) = out.split_at_mut(available);
            if let Some(physical) = run.physical {
                let offset = physical
                    .checked_add(run_offset)
                    .ok_or(ListStreamError::Invalid)?;
                self.reader.seek(SeekFrom::Start(offset))?;
                self.reader.read_exact(head)?;
            } else {
                head.fill(0);
            }
            self.position = self
                .position
                .checked_add(available as u64)
                .ok_or(ListStreamError::Invalid)?;
            out = tail;
        }
        Ok(())
    }
}

/// Stream a non-resident list entry-by-entry. Entry lengths are `u16`, so the
/// wire buffer is fixed at 64KiB; a compact identity set additionally proves
/// that no target/instance pair occurs twice.
///
/// `prefix` accepts an incomplete final entry when the caller is reading only
/// the base extent to discover continuation extents. The complete pass rejects
/// the same truncation.
///
/// # Errors
///
/// [`ListStreamError::Io`] if the reader fails, [`ListStreamError::Cancelled`]
/// if `stop` is set, and [`ListStreamError::Invalid`] for any entry sequence
/// that is malformed, truncated (outside `prefix` mode), repeats a
/// target/instance pair, or exceeds the per-list entry caps.
pub fn visit_list_stream(
    reader: &mut (impl Read + Seek),
    runs: &[StreamRun],
    data_size: u64,
    stop: &AtomicBool,
    prefix: bool,
    mut visit: impl FnMut(ListEntry),
) -> Result<(), ListStreamError> {
    let header_len = ATTRIBUTE_LIST_ENTRY_BYTES;
    let mut stream = RunReader::new(reader, runs, data_size)?;
    let mut header = [0u8; ATTRIBUTE_LIST_ENTRY_BYTES];
    let mut scratch = vec![0u8; STREAM_BUFFER_BYTES];
    let mut sequence = EntrySequence::default();
    let mut visited = 0usize;
    while stream.remaining() > 0 {
        if stop.load(Ordering::Relaxed) {
            return Err(ListStreamError::Cancelled);
        }
        if stream.remaining() < header_len as u64 {
            let tail_len =
                usize::try_from(stream.remaining()).map_err(|_| ListStreamError::Invalid)?;
            let mut tail = [0u8; ATTRIBUTE_LIST_ENTRY_BYTES - 1];
            stream.read_exact(&mut tail[..tail_len])?;
            if prefix {
                return Ok(());
            }
            return Err(ListStreamError::Invalid);
        }
        stream.read_exact(&mut header)?;
        let len = declared_entry_length(&header).ok_or(ListStreamError::Invalid)?;
        validate_entry_header(&header, len).ok_or(ListStreamError::Invalid)?;
        let after_header = u64::try_from(len)
            .map_err(|_| ListStreamError::Invalid)?
            .checked_sub(header_len as u64)
            .ok_or(ListStreamError::Invalid)?;
        if after_header > stream.remaining() {
            return if prefix {
                Ok(())
            } else {
                Err(ListStreamError::Invalid)
            };
        }
        scratch[..header_len].copy_from_slice(&header);
        stream.read_exact(&mut scratch[header_len..len])?;
        let decoded = decode_list_entry(&scratch[..len]).ok_or(ListStreamError::Invalid)?;
        if !sequence.accept(&decoded) {
            return Err(ListStreamError::Invalid);
        }
        visit(decoded.public);
        visited += 1;
    }
    if prefix || visited != 0 {
        Ok(())
    } else {
        Err(ListStreamError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::super::ntfs::NtfsAttributeType;
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn runs_may_extend_past_the_stream_because_ntfs_allocates_whole_clusters() {
        // Mapping pairs describe the ALLOCATED extent. NTFS allocates whole
        // clusters and does not release the tail when a stream shrinks, so a
        // 96-byte $ATTRIBUTE_LIST holding two 4KiB clusters is ordinary. The
        // tail run is never read — `RunReader` bounds itself by `data_size` —
        // but rejecting it failed a real C: outright.
        let runs = vec![
            StreamRun {
                logical: 0,
                physical: Some(1 << 20),
                len: 4096,
            },
            StreamRun {
                logical: 4096,
                physical: Some(2 << 20),
                len: 4096,
            },
        ];
        assert!(partial_runs_are_valid(&runs, 96));
        assert!(complete_runs_are_valid(&runs, 96));
        // A gap before the stream is covered is still a rejection.
        let holed = vec![StreamRun {
            logical: 4096,
            physical: Some(1 << 20),
            len: 4096,
        }];
        assert!(!partial_runs_are_valid(&holed, 96));
    }

    fn put_u16(data: &mut [u8], off: usize, value: u16) {
        data[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(data: &mut [u8], off: usize, value: u32) {
        data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(data: &mut [u8], off: usize, value: u64) {
        data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    const fn frn(record: u64) -> u64 {
        (1u64 << 48) | record
    }

    fn list_entry_named(type_id: u32, name: &[u16], vcn: u64, target: u64, id: u16) -> Vec<u8> {
        let record_len = (ATTRIBUTE_LIST_ENTRY_BYTES + size_of_val(name)).next_multiple_of(8);
        let mut data = vec![0u8; record_len];
        put_u32(&mut data, 0, type_id);
        put_u16(&mut data, 4, record_len as u16);
        data[6] = name.len() as u8;
        if !name.is_empty() {
            data[7] = ATTRIBUTE_LIST_ENTRY_BYTES as u8;
            for (index, unit) in name.iter().enumerate() {
                put_u16(
                    &mut data,
                    ATTRIBUTE_LIST_ENTRY_BYTES + index * size_of::<u16>(),
                    *unit,
                );
            }
        }
        put_u64(&mut data, 8, vcn);
        let target = if target >> 48 == 0 {
            frn(target)
        } else {
            target
        };
        put_u64(&mut data, 16, target);
        put_u16(&mut data, 24, id);
        data
    }

    fn list_entry(type_id: u32, vcn: u64, target: u64, id: u16) -> Vec<u8> {
        list_entry_named(type_id, &[], vcn, target, id)
    }

    fn nonresident_attr(lowest: u64, highest: u64, data_size: u64, mapping: &[u8]) -> Vec<u8> {
        let header = NONRESIDENT_HEADER_BYTES;
        let attr_len = (header + mapping.len()).next_multiple_of(8);
        let mut attr = vec![0u8; attr_len];
        put_u32(&mut attr, 0, NtfsAttributeType::AttributeList as u32);
        put_u32(&mut attr, 4, attr_len as u32);
        attr[8] = 1;
        put_u64(&mut attr, 16, lowest);
        put_u64(&mut attr, 24, highest);
        put_u16(&mut attr, 32, header as u16);
        put_u64(&mut attr, 40, highest.saturating_add(1).saturating_mul(4));
        put_u64(&mut attr, 48, data_size);
        put_u64(&mut attr, 56, data_size);
        attr[header..header + mapping.len()].copy_from_slice(mapping);
        attr
    }

    #[test]
    fn complete_and_prefix_entry_parsing_fail_closed() {
        let first = list_entry(0x20, 0, (3u64 << 48) | 0x2A, 7);
        let second = list_entry(0x30, 0, (4u64 << 48) | 0x63, 8);
        let mut all = first;
        all.extend_from_slice(&second);
        let parsed = parse_list_entries(&all, false).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].target_record(), 42);
        assert_eq!(parsed[1].target_reference, (4u64 << 48) | 0x63);

        let cut = &all[..all.len() - 3];
        assert!(parse_list_entries(cut, false).is_none());
        assert_eq!(parse_list_entries(cut, true).unwrap().len(), 1);

        let mut bad_name =
            list_entry_named(NtfsAttributeType::Data as u32, &[b'x' as u16], 0, 7, 3);
        bad_name[7] = 31;
        assert!(parse_list_entries(&bad_name, false).is_none());
    }

    #[test]
    fn record_length_is_aligned_and_is_the_exact_advance() {
        let entry = list_entry(NtfsAttributeType::AttributeList as u32, 0, 1, 1);

        let mut unaligned = entry.clone();
        put_u16(&mut unaligned, 4, 30);
        assert!(parse_list_entries(&unaligned, false).is_none());
        assert!(parse_list_entries(&unaligned, true).is_none());

        let mut declared_past_end = entry.clone();
        put_u16(&mut declared_past_end, 4, 40);
        assert!(parse_list_entries(&declared_past_end, false).is_none());
        assert!(
            parse_list_entries(&declared_past_end, true)
                .unwrap()
                .is_empty()
        );

        let mut zero_tail = entry;
        zero_tail.extend_from_slice(&[0; 8]);
        assert!(parse_list_entries(&zero_tail, false).is_none());
        assert_eq!(parse_list_entries(&zero_tail, true).unwrap().len(), 1);
        assert!(parse_list_entries(&[], false).is_none());
    }

    #[test]
    fn relevant_entries_are_unnamed_and_file_name_vcn_is_zero() {
        let named_list = list_entry_named(
            NtfsAttributeType::AttributeList as u32,
            &[b'x' as u16],
            0,
            1,
            1,
        );
        assert!(parse_list_entries(&named_list, false).is_none());

        let named_file =
            list_entry_named(NtfsAttributeType::FileName as u32, &[b'x' as u16], 0, 2, 2);
        assert!(parse_list_entries(&named_file, false).is_none());

        let continuation_name = list_entry(NtfsAttributeType::FileName as u32, 1, 3, 3);
        assert!(parse_list_entries(&continuation_name, false).is_none());

        let named_data = list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'a' as u16, b'd' as u16, b's' as u16],
            0,
            4,
            4,
        );
        let parsed = parse_list_entries(&named_data, false).unwrap();
        assert_eq!(parsed[0].name_length, 3);

        for invalid_type in [0, NtfsAttributeType::End as u32, 0x11, 0x110] {
            let invalid = list_entry(invalid_type, 0, 5, 5);
            assert!(parse_list_entries(&invalid, false).is_none());
        }

        let mut zero_sequence = list_entry(NtfsAttributeType::AttributeList as u32, 0, 5, 5);
        put_u64(&mut zero_sequence, 16, 5);
        assert!(parse_list_entries(&zero_sequence, false).is_none());
    }

    #[test]
    fn provable_ordering_and_both_uniqueness_keys_are_strict() {
        let mut descending_type = list_entry(NtfsAttributeType::Data as u32, 0, 1, 1);
        descending_type.extend_from_slice(&list_entry(NtfsAttributeType::FileName as u32, 0, 2, 2));
        assert!(parse_list_entries(&descending_type, false).is_none());

        let mut raw_descending_name =
            list_entry_named(NtfsAttributeType::Data as u32, &[b'b' as u16], 0, 1, 1);
        raw_descending_name.extend_from_slice(&list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'a' as u16],
            0,
            2,
            2,
        ));
        assert_eq!(
            parse_list_entries(&raw_descending_name, false)
                .unwrap()
                .len(),
            2
        );

        let mut descending_vcn_for_same_raw_name =
            list_entry_named(NtfsAttributeType::Data as u32, &[b'x' as u16], 2, 1, 1);
        descending_vcn_for_same_raw_name.extend_from_slice(&list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'y' as u16],
            0,
            3,
            3,
        ));
        descending_vcn_for_same_raw_name.extend_from_slice(&list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'x' as u16],
            1,
            2,
            2,
        ));
        assert!(parse_list_entries(&descending_vcn_for_same_raw_name, false).is_none());

        let mut duplicate_key =
            list_entry_named(NtfsAttributeType::Data as u32, &[b'x' as u16], 1, 1, 1);
        duplicate_key.extend_from_slice(&list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'x' as u16],
            1,
            2,
            2,
        ));
        assert!(parse_list_entries(&duplicate_key, false).is_none());

        let mut duplicate_target_instance =
            list_entry(NtfsAttributeType::AttributeList as u32, 0, 7, 3);
        duplicate_target_instance.extend_from_slice(&list_entry(
            NtfsAttributeType::FileName as u32,
            0,
            7,
            3,
        ));
        assert!(parse_list_entries(&duplicate_target_instance, false).is_none());
    }

    /// Replay the same bytes through the streaming entry point, which shares
    /// `EntrySequence` with `parse_list_entries`. Both callers must agree.
    fn stream_entries(bytes: &[u8]) -> Option<Vec<ListEntry>> {
        let runs = [StreamRun {
            logical: 0,
            physical: Some(0),
            len: bytes.len() as u64,
        }];
        let stop = AtomicBool::new(false);
        let mut cursor = std::io::Cursor::new(bytes.to_vec());
        let mut visited = Vec::new();
        visit_list_stream(&mut cursor, &runs, runs[0].len, &stop, false, |entry| {
            visited.push(entry);
        })
        .ok()
        .map(|()| visited)
    }

    #[test]
    fn several_hard_links_keep_every_file_name_entry() {
        // One file, three links living in three different extension records.
        // Instance ids only have to be unique inside their own record, so two
        // of them deliberately collide across records.
        let mut list = list_entry(NtfsAttributeType::FileName as u32, 0, frn(30), 1);
        list.extend_from_slice(&list_entry(
            NtfsAttributeType::FileName as u32,
            0,
            frn(31),
            1,
        ));
        list.extend_from_slice(&list_entry(
            NtfsAttributeType::FileName as u32,
            0,
            frn(32),
            4,
        ));

        for parsed in [
            parse_list_entries(&list, false).expect("hard links must parse"),
            stream_entries(&list).expect("hard links must stream"),
        ] {
            assert_eq!(
                parsed
                    .iter()
                    .map(|entry| (entry.target_record(), entry.id))
                    .collect::<Vec<_>>(),
                vec![(30, 1), (31, 1), (32, 4)],
            );
        }
    }

    #[test]
    fn a_long_name_and_its_dos_pair_share_one_record() {
        // The WinSxS steady state: both `$FILE_NAME` attributes sit in the same
        // target record and are told apart only by their instance id.
        let mut list = list_entry(NtfsAttributeType::FileName as u32, 0, frn(30), 2);
        list.extend_from_slice(&list_entry(
            NtfsAttributeType::FileName as u32,
            0,
            frn(30),
            3,
        ));

        for parsed in [
            parse_list_entries(&list, false).expect("a DOS pair must parse"),
            stream_entries(&list).expect("a DOS pair must stream"),
        ] {
            assert_eq!(parsed.len(), 2);
            assert_eq!(parsed[0].target_reference, parsed[1].target_reference);
            assert_eq!((parsed[0].id, parsed[1].id), (2, 3));
        }
    }

    #[test]
    fn file_name_entries_stay_bounded_by_identity_and_by_fan_out() {
        let mut identical = list_entry(NtfsAttributeType::FileName as u32, 0, frn(30), 2);
        identical.extend_from_slice(&list_entry(
            NtfsAttributeType::FileName as u32,
            0,
            frn(30),
            2,
        ));
        assert!(parse_list_entries(&identical, false).is_none());
        assert!(stream_entries(&identical).is_none());

        let mut budgeted = Vec::new();
        for index in 0..MAX_FILE_NAME_ENTRIES {
            budgeted.extend_from_slice(&list_entry(
                NtfsAttributeType::FileName as u32,
                0,
                frn(index as u64 + 1),
                1,
            ));
        }
        assert_eq!(
            parse_list_entries(&budgeted, false).map(|entries| entries.len()),
            Some(MAX_FILE_NAME_ENTRIES),
        );

        budgeted.extend_from_slice(&list_entry(
            NtfsAttributeType::FileName as u32,
            0,
            frn(MAX_FILE_NAME_ENTRIES as u64 + 1),
            1,
        ));
        assert!(parse_list_entries(&budgeted, false).is_none());
        assert!(stream_entries(&budgeted).is_none());
    }

    #[test]
    fn split_extents_still_require_strictly_increasing_vcns() {
        // Unnamed entries take the same `(type, name)` key `$FILE_NAME` would
        // have taken, so this is the exact rule the relaxation must not touch.
        let ascending = |first: u64, second: u64| {
            let mut list = list_entry(NtfsAttributeType::Data as u32, first, frn(40), 1);
            list.extend_from_slice(&list_entry(
                NtfsAttributeType::Data as u32,
                second,
                frn(41),
                2,
            ));
            list
        };

        assert_eq!(
            parse_list_entries(&ascending(0, 4), false).map(|entries| entries.len()),
            Some(2),
        );

        for reversed_or_repeated in [ascending(4, 0), ascending(4, 4)] {
            assert!(parse_list_entries(&reversed_or_repeated, false).is_none());
            assert!(stream_entries(&reversed_or_repeated).is_none());
        }

        let mut interleaved_named = list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'x' as u16],
            2,
            frn(40),
            1,
        );
        interleaved_named.extend_from_slice(&list_entry(
            NtfsAttributeType::Data as u32,
            0,
            frn(41),
            2,
        ));
        interleaved_named.extend_from_slice(&list_entry_named(
            NtfsAttributeType::Data as u32,
            &[b'x' as u16],
            1,
            frn(42),
            3,
        ));
        assert!(parse_list_entries(&interleaved_named, false).is_none());
    }

    #[test]
    fn run_decoder_handles_relative_and_negative_lcn_deltas() {
        // 2 clusters at LCN 10, one at LCN 12, then one at LCN 8
        // (delta -4 from the previous physical LCN).
        let mapping = [0x11, 2, 10, 0x11, 1, 2, 0x11, 1, 0xFC, 0];
        let bytes = nonresident_attr(0, 3, 13, &mapping);
        let attr = NtfsAttribute::parse(&bytes).unwrap();
        let (size, runs) = decode_extent_runs(&attr, 4, 1024).unwrap();
        assert_eq!(size, 13);
        assert_eq!(
            runs,
            vec![
                StreamRun {
                    logical: 0,
                    physical: Some(40),
                    len: 8,
                },
                StreamRun {
                    logical: 8,
                    physical: Some(48),
                    len: 4,
                },
                StreamRun {
                    logical: 12,
                    physical: Some(32),
                    len: 4,
                },
            ]
        );
    }

    #[test]
    fn run_decoder_rejects_sparse_unterminated_mismatched_and_out_of_volume_runs() {
        let sparse = nonresident_attr(0, 0, 4, &[0x01, 1, 0]);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&sparse).unwrap(), 4, 1024).is_none());

        let unterminated = nonresident_attr(0, 2, 12, &[0x11, 1, 1, 0x11, 1, 1, 0x11, 1]);
        assert!(
            decode_extent_runs(&NtfsAttribute::parse(&unterminated).unwrap(), 4, 1024).is_none()
        );

        let wrong_vcn_span = nonresident_attr(0, 2, 4, &[0x11, 1, 1, 0]);
        assert!(
            decode_extent_runs(&NtfsAttribute::parse(&wrong_vcn_span).unwrap(), 4, 1024).is_none()
        );

        let outside = nonresident_attr(0, 0, 4, &[0x11, 1, 0x7F, 0]);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&outside).unwrap(), 4, 64).is_none());

        let aliased = nonresident_attr(0, 2, 12, &[0x11, 2, 10, 0x11, 1, 1, 0]);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&aliased).unwrap(), 4, 1024).is_none());
    }

    #[test]
    fn run_decoder_rejects_invalid_system_stream_metadata() {
        let valid = nonresident_attr(0, 0, 4, &[0x11, 1, 1, 0]);
        let parsed = NtfsAttribute::parse(&valid).unwrap();
        assert!(decode_extent_runs(&parsed, 0, 1024).is_none());
        assert!(decode_extent_runs(&parsed, 3, 1024).is_none());
        assert!(decode_extent_runs(&parsed, 4, 0).is_none());

        let mut named = valid.clone();
        named[9] = 1;
        put_u16(&mut named, 10, NONRESIDENT_HEADER_BYTES as u16);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&named).unwrap(), 4, 1024).is_none());

        for flags in [0x0001, 0x4000, 0x8000] {
            let mut flagged = valid.clone();
            put_u16(&mut flagged, 12, flags);
            assert!(
                decode_extent_runs(&NtfsAttribute::parse(&flagged).unwrap(), 4, 1024).is_none()
            );
        }

        let mut wrong_type = valid.clone();
        put_u32(&mut wrong_type, 0, NtfsAttributeType::FileName as u32);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&wrong_type).unwrap(), 4, 1024).is_none());

        let mut unaligned_record = valid.clone();
        put_u32(&mut unaligned_record, 4, (valid.len() - 1) as u32);
        assert!(
            decode_extent_runs(&NtfsAttribute::parse(&unaligned_record).unwrap(), 4, 1024,)
                .is_none()
        );

        let mut reserved = valid.clone();
        reserved[34] = 1;
        assert!(decode_extent_runs(&NtfsAttribute::parse(&reserved).unwrap(), 4, 1024).is_none());

        let mut zero_allocation = valid.clone();
        put_u64(&mut zero_allocation, 40, 0);
        assert!(
            decode_extent_runs(&NtfsAttribute::parse(&zero_allocation).unwrap(), 4, 1024).is_none()
        );

        let mut zero_data = valid.clone();
        put_u64(&mut zero_data, 48, 0);
        put_u64(&mut zero_data, 56, 0);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&zero_data).unwrap(), 4, 1024).is_none());

        let mut unaligned_allocation = valid.clone();
        put_u64(&mut unaligned_allocation, 40, 5);
        assert!(
            decode_extent_runs(
                &NtfsAttribute::parse(&unaligned_allocation).unwrap(),
                4,
                1024,
            )
            .is_none()
        );

        let mut allocation_past_volume = valid.clone();
        put_u64(&mut allocation_past_volume, 40, 2048);
        assert!(
            decode_extent_runs(
                &NtfsAttribute::parse(&allocation_past_volume).unwrap(),
                4,
                1024,
            )
            .is_none()
        );

        let mut extent_past_allocation = nonresident_attr(0, 1, 4, &[0x11, 2, 1, 0]);
        put_u64(&mut extent_past_allocation, 40, 4);
        assert!(
            decode_extent_runs(
                &NtfsAttribute::parse(&extent_past_allocation).unwrap(),
                4,
                1024,
            )
            .is_none()
        );

        let mut oversized_data = valid.clone();
        put_u64(&mut oversized_data, 48, 5);
        put_u64(&mut oversized_data, 56, 5);
        assert!(
            decode_extent_runs(&NtfsAttribute::parse(&oversized_data).unwrap(), 4, 1024).is_none()
        );

        let mut initialized_past_data = valid.clone();
        put_u64(&mut initialized_past_data, 56, 5);
        assert!(
            decode_extent_runs(
                &NtfsAttribute::parse(&initialized_past_data).unwrap(),
                4,
                1024,
            )
            .is_none()
        );

        let mut shifted_mapping = valid.clone();
        put_u16(
            &mut shifted_mapping,
            32,
            (NONRESIDENT_HEADER_BYTES + 1) as u16,
        );
        assert!(
            decode_extent_runs(&NtfsAttribute::parse(&shifted_mapping).unwrap(), 4, 1024).is_none()
        );

        let lcn_zero = nonresident_attr(0, 0, 4, &[0x11, 1, 0, 0]);
        assert!(decode_extent_runs(&NtfsAttribute::parse(&lcn_zero).unwrap(), 4, 1024).is_none());

        let mut oversized_list = valid.clone();
        put_u64(&mut oversized_list, 40, MAX_LIST_BYTES + 4);
        put_u64(&mut oversized_list, 48, MAX_LIST_BYTES + 1);
        put_u64(&mut oversized_list, 56, MAX_LIST_BYTES + 1);
        assert!(
            decode_extent_runs(
                &NtfsAttribute::parse(&oversized_list).unwrap(),
                4,
                MAX_LIST_BYTES + 8,
            )
            .is_none()
        );

        put_u32(&mut oversized_list, 0, NtfsAttributeType::Data as u32);
        assert!(
            decode_extent_runs(
                &NtfsAttribute::parse(&oversized_list).unwrap(),
                4,
                MAX_LIST_BYTES + 8,
            )
            .is_some(),
            "the list budget must not cap the machine-wide $MFT stream"
        );
    }

    #[test]
    fn stream_parser_crosses_fragmented_runs_and_rejects_zero_tail() {
        let first = list_entry(0x20, 0, (1u64 << 48) | 0x1E, 1);
        let second = list_entry(0x30, 0, (1u64 << 48) | 0x1F, 2);
        assert_eq!((first.len(), second.len()), (32, 32));
        let mut physical = vec![0u8; 96];
        physical[64..96].copy_from_slice(&first);
        physical[0..32].copy_from_slice(&second);
        let mut backing = std::io::Cursor::new(physical);
        let runs = vec![
            StreamRun {
                logical: 0,
                physical: Some(64),
                len: 32,
            },
            StreamRun {
                logical: 32,
                physical: Some(0),
                len: 32,
            },
            StreamRun {
                logical: 64,
                physical: Some(32),
                len: 8,
            },
        ];
        let stop = AtomicBool::new(false);
        let mut got = Vec::new();
        // The list is exactly the first two (out-of-order, non-adjacent) runs:
        // 64 bytes of stream covered by 64 bytes of runs. The third run is the
        // zero tail exercised below, and a run may not start at or past
        // `data_size`, so it is not part of the well-formed case.
        visit_list_stream(&mut backing, &runs[..2], 64, &stop, false, |entry| {
            got.push(entry);
        })
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].target_record(), 30);
        assert_eq!(got[1].target_record(), 31);

        assert!(matches!(
            visit_list_stream(&mut backing, &runs, 72, &stop, false, |_| {}),
            Err(ListStreamError::Invalid)
        ));
        stop.store(true, Ordering::Relaxed);
        assert!(matches!(
            visit_list_stream(&mut backing, &runs, 72, &stop, false, |_| {}),
            Err(ListStreamError::Cancelled)
        ));
    }

    #[test]
    fn stream_parser_enforces_prefix_and_sequence_rules_too() {
        let first = list_entry(NtfsAttributeType::AttributeList as u32, 0, 1, 1);
        let second = list_entry(NtfsAttributeType::AttributeList as u32, 1, 2, 2);
        let mut valid = first.clone();
        valid.extend_from_slice(&second);
        let cut_len = valid.len() - 3;
        let runs = [StreamRun {
            logical: 0,
            physical: Some(0),
            len: valid.len() as u64,
        }];
        let stop = AtomicBool::new(false);
        let mut cursor = std::io::Cursor::new(valid);
        let mut visited = Vec::new();
        visit_list_stream(&mut cursor, &runs, cut_len as u64, &stop, true, |entry| {
            visited.push(entry);
        })
        .unwrap();
        assert_eq!(visited, vec![parse_list_entries(&first, false).unwrap()[0]]);

        let mut duplicate = first;
        duplicate.extend_from_slice(&list_entry(NtfsAttributeType::FileName as u32, 0, 1, 1));
        let duplicate_runs = [StreamRun {
            logical: 0,
            physical: Some(0),
            len: duplicate.len() as u64,
        }];
        let mut duplicate_cursor = std::io::Cursor::new(duplicate);
        assert!(matches!(
            visit_list_stream(
                &mut duplicate_cursor,
                &duplicate_runs,
                duplicate_runs[0].len,
                &stop,
                false,
                |_| {},
            ),
            Err(ListStreamError::Invalid)
        ));

        let aliased_runs = [
            StreamRun {
                logical: 0,
                physical: Some(0),
                len: 32,
            },
            StreamRun {
                logical: 32,
                physical: Some(16),
                len: 32,
            },
        ];
        let mut aliased_cursor = std::io::Cursor::new(vec![0; 64]);
        assert!(matches!(
            visit_list_stream(&mut aliased_cursor, &aliased_runs, 64, &stop, false, |_| {},),
            Err(ListStreamError::Invalid)
        ));

        let extra_logical_run = [
            StreamRun {
                logical: 0,
                physical: Some(0),
                len: 32,
            },
            StreamRun {
                logical: 32,
                physical: Some(32),
                len: 32,
            },
        ];
        let mut extra_cursor = std::io::Cursor::new(vec![0; 64]);
        assert!(matches!(
            visit_list_stream(
                &mut extra_cursor,
                &extra_logical_run,
                32,
                &stop,
                false,
                |_| {},
            ),
            Err(ListStreamError::Invalid)
        ));
    }

    #[test]
    fn stream_parser_supports_large_lists_within_the_explicit_budget() {
        let entry = list_entry(NtfsAttributeType::AttributeList as u32, 0, 1, 1);
        let count = ((4usize << 20) / entry.len()) + 2;
        let mut bytes = Vec::with_capacity(count * entry.len());
        for index in 0..count {
            bytes.extend_from_slice(&list_entry(
                NtfsAttributeType::AttributeList as u32,
                index as u64,
                index as u64 + 1,
                1,
            ));
        }
        assert!(bytes.len() > 4usize << 20);
        let data_size = bytes.len() as u64;
        let runs = [StreamRun {
            logical: 0,
            physical: Some(0),
            len: data_size,
        }];
        let stop = AtomicBool::new(false);
        let mut cursor = std::io::Cursor::new(bytes);
        let mut visited = 0usize;
        visit_list_stream(&mut cursor, &runs, data_size, &stop, false, |_| {
            visited += 1;
        })
        .unwrap();
        assert_eq!(visited, count);
    }

    #[test]
    fn parser_resource_limits_fail_before_unbounded_growth() {
        let oversized = vec![0u8; MAX_LIST_BYTES as usize + 1];
        assert!(parse_list_entries(&oversized, false).is_none());

        let entry = list_entry(NtfsAttributeType::AttributeList as u32, 0, 1, 1);
        let mut too_many = Vec::with_capacity((MAX_LIST_ENTRIES + 1) * entry.len());
        for index in 0..=MAX_LIST_ENTRIES {
            too_many.extend_from_slice(&list_entry(
                NtfsAttributeType::AttributeList as u32,
                index as u64,
                index as u64 + 1,
                1,
            ));
        }
        assert!(parse_list_entries(&too_many, false).is_none());

        let too_many_runs: Vec<_> = (0..=MAX_LIST_RUNS)
            .map(|index| StreamRun {
                logical: index as u64,
                physical: Some(index as u64),
                len: 1,
            })
            .collect();
        let mut cursor = std::io::Cursor::new(vec![0; MAX_LIST_RUNS + 1]);
        let stop = AtomicBool::new(false);
        assert!(matches!(
            visit_list_stream(
                &mut cursor,
                &too_many_runs,
                (MAX_LIST_RUNS + 1) as u64,
                &stop,
                false,
                |_| {},
            ),
            Err(ListStreamError::Invalid)
        ));
    }

    #[test]
    fn extent_closure_discovers_continuations_revealed_by_later_extents() {
        let base = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 0, frn(10), 1);
        let second = list_entry(NtfsAttributeType::AttributeList as u32, 1, 11, 2);
        let third = list_entry(NtfsAttributeType::AttributeList as u32, 2, 12, 3);
        let tail = list_entry(NtfsAttributeType::FileName as u32, 0, 13, 4);
        let mut bytes = second;
        bytes.extend_from_slice(&third);
        bytes.extend_from_slice(&tail);
        let mut backing = std::io::Cursor::new(bytes);
        let stop = AtomicBool::new(false);
        let initial = vec![StreamRun {
            logical: 0,
            physical: Some(0),
            len: 32,
        }];
        let mut discoveries = 0;
        let runs = close_extent_runs(
            initial,
            96,
            base,
            |runs, prefix| {
                discoveries += 1;
                let mut entries = Vec::new();
                visit_list_stream(&mut backing, runs, prefix, &stop, true, |entry| {
                    if entry.type_id == NtfsAttributeType::AttributeList as u32 {
                        entries.push(entry);
                    }
                })
                .ok()?;
                Some(entries)
            },
            |entry| {
                let logical = entry.starting_vcn.checked_mul(32)?;
                Some(vec![StreamRun {
                    logical,
                    physical: Some(logical),
                    len: 32,
                }])
            },
        )
        .unwrap();
        assert_eq!(discoveries, 2);
        assert_eq!(covered_prefix(&runs), 96);
    }

    #[test]
    fn extent_closure_fails_when_a_prefix_cannot_reveal_its_successor() {
        let base = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 0, frn(10), 1);
        let initial = vec![StreamRun {
            logical: 0,
            physical: Some(100),
            len: 32,
        }];
        assert!(close_extent_runs(initial, 64, base, |_, _| Some(vec![base]), |_| None).is_none());
    }

    #[test]
    fn extent_closure_rejects_target_instance_aliases() {
        let base = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 0, frn(10), 1);
        let alias = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 1, frn(10), 1);
        let initial = vec![StreamRun {
            logical: 0,
            physical: Some(100),
            len: 32,
        }];
        let mut decoded_alias = false;
        assert!(
            close_extent_runs(
                initial,
                64,
                base,
                |_, _| Some(vec![alias]),
                |_| {
                    decoded_alias = true;
                    Some(vec![])
                },
            )
            .is_none()
        );
        assert!(!decoded_alias);
    }

    #[test]
    fn extent_closure_rejects_cross_extent_physical_aliases() {
        let base = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 0, frn(10), 1);
        let continuation =
            ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 1, frn(11), 2);
        let initial = vec![StreamRun {
            logical: 0,
            physical: Some(100),
            len: 32,
        }];
        assert!(
            close_extent_runs(
                initial,
                64,
                base,
                |_, _| Some(vec![continuation]),
                |_| Some(vec![StreamRun {
                    logical: 32,
                    physical: Some(116),
                    len: 32,
                }]),
            )
            .is_none()
        );
    }

    #[test]
    fn extent_closure_has_explicit_pass_and_scan_work_budgets() {
        const MIB: u64 = 1 << 20;

        let base = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 0, frn(10), 1);
        let initial = vec![StreamRun {
            logical: 0,
            physical: Some(0),
            len: 32,
        }];
        let mut next = 1u64;
        let mut discoveries = 0usize;
        assert!(
            close_extent_runs(
                initial,
                (MAX_CLOSURE_PASSES as u64 + 2) * 32,
                base,
                |_, _| {
                    discoveries += 1;
                    let entry = ListEntry::unnamed(
                        NtfsAttributeType::AttributeList as u32,
                        next,
                        frn(next + 10),
                        u16::try_from(next).ok()?,
                    );
                    next += 1;
                    Some(vec![entry])
                },
                |entry| Some(vec![StreamRun {
                    logical: entry.starting_vcn * 32,
                    physical: Some(10_000 + entry.starting_vcn * 32),
                    len: 32,
                }]),
            )
            .is_none()
        );
        assert_eq!(discoveries, MAX_CLOSURE_PASSES);

        let next_extent = std::cell::Cell::new(1u64);
        let large_initial = vec![StreamRun {
            logical: 0,
            physical: Some(0),
            len: 8 * MIB,
        }];
        assert!(
            close_extent_runs(
                large_initial,
                MAX_LIST_BYTES,
                base,
                |_, _| {
                    let index = next_extent.get();
                    next_extent.set(index + 1);
                    Some(vec![ListEntry::unnamed(
                        NtfsAttributeType::AttributeList as u32,
                        index,
                        frn(index + 100),
                        u16::try_from(index).ok()?,
                    )])
                },
                |entry| Some(vec![StreamRun {
                    logical: 8 * MIB + (entry.starting_vcn - 1) * MIB,
                    physical: Some(32 * MIB + entry.starting_vcn * MIB),
                    len: MIB,
                }]),
            )
            .is_none()
        );
    }

    proptest! {
        #[test]
        fn arbitrary_entry_bytes_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
            prefix in any::<bool>(),
        ) {
            let _ = parse_list_entries(&bytes, prefix);
            let runs = if bytes.is_empty() {
                Vec::new()
            } else {
                vec![StreamRun {
                    logical: 0,
                    physical: Some(0),
                    len: bytes.len() as u64,
                }]
            };
            let stop = AtomicBool::new(false);
            let mut cursor = std::io::Cursor::new(bytes.clone());
            let _ = visit_list_stream(
                &mut cursor,
                &runs,
                bytes.len() as u64,
                &stop,
                prefix,
                |_| {},
            );
        }
    }
}
