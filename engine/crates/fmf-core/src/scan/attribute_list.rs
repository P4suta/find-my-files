//! Checked primitives shared by initial-scan and live-USN `$ATTRIBUTE_LIST`
//! resolution. This module owns the untrusted byte grammar and non-resident
//! run arithmetic; callers own record acquisition and name policy.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use ntfs_reader::api::{NtfsAttributeListEntry, NtfsNonResidentAttributeHeader};
use ntfs_reader::attribute::NtfsAttribute;
use rustc_hash::FxHashSet;

const FILE_REFERENCE_RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const STREAM_BUFFER_BYTES: usize = 64 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListEntry {
    pub type_id: u32,
    pub starting_vcn: u64,
    pub target_reference: u64,
    pub id: u16,
}

impl ListEntry {
    pub const fn target_record(self) -> u64 {
        self.target_reference & FILE_REFERENCE_RECORD_MASK
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRun {
    pub logical: u64,
    pub physical: Option<u64>,
    pub len: u64,
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

/// Parse owned entry fields without forming a reference to packed,
/// potentially unaligned disk bytes. `prefix` permits only an incomplete last
/// entry (needed while discovering a non-resident list's continuation
/// extents); a complete stream fails closed on the same bytes.
pub fn parse_list_entries(data: &[u8], prefix: bool) -> Option<Vec<ListEntry>> {
    let header_len = size_of::<NtfsAttributeListEntry>();
    let mut entries = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        let remaining = &data[off..];
        if remaining.len() < header_len {
            if remaining.iter().all(|&byte| byte == 0) {
                break;
            }
            return prefix.then_some(entries);
        }
        let len = le_u16(remaining, 4) as usize;
        if len < header_len {
            return None;
        }
        let entry_end = off.checked_add(len)?;
        if entry_end > data.len() {
            return prefix.then_some(entries);
        }
        let name_len = remaining[6] as usize;
        let name_off = remaining[7] as usize;
        let name_bytes = name_len.checked_mul(2)?;
        if name_len > 0
            && (name_off < header_len
                || name_off.checked_add(name_bytes).is_none_or(|end| end > len))
        {
            return None;
        }
        entries.push(ListEntry {
            type_id: le_u32(remaining, 0),
            starting_vcn: le_u64(remaining, 8),
            target_reference: le_u64(remaining, 16),
            id: le_u16(remaining, 24),
        });
        let padded = len.checked_next_multiple_of(8)?;
        let next = off.checked_add(padded)?;
        if next > data.len() {
            return prefix.then_some(entries);
        }
        off = next;
    }
    Some(entries)
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
/// All logical/physical arithmetic and volume bounds are checked before I/O.
pub fn decode_extent_runs(
    attr: &NtfsAttribute<'_>,
    cluster_size: u64,
    volume_size: u64,
) -> Option<(u64, Vec<StreamRun>)> {
    if cluster_size == 0 || volume_size == 0 {
        return None;
    }
    let header = attr.nonresident_header()?;
    let lowest_vcn = u64::try_from(header.lowest_vcn).ok()?;
    let highest_vcn = u64::try_from(header.highest_vcn).ok()?;
    let extent_clusters = highest_vcn.checked_sub(lowest_vcn)?.checked_add(1)?;
    let logical_base = lowest_vcn.checked_mul(cluster_size)?;
    let data_size = header.data_size;
    let runs_start = header.data_runs_offset as usize;
    if runs_start < size_of::<NtfsNonResidentAttributeHeader>() || runs_start >= attr.data().len() {
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
        let physical = if offset_bytes == 0 {
            None
        } else {
            let offset_end = cursor.checked_add(offset_bytes)?;
            let delta = i128::from(signed_le(mapping.get(cursor..offset_end)?));
            cursor = offset_end;
            previous_lcn = previous_lcn.checked_add(delta)?;
            let lcn = u64::try_from(previous_lcn).ok()?;
            let physical = lcn.checked_mul(cluster_size)?;
            if physical.checked_add(run_len)? > volume_size {
                return None;
            }
            Some(physical)
        };
        runs.push(StreamRun {
            logical,
            physical,
            len: run_len,
        });
    }
    if logical_in_extent != extent_clusters.checked_mul(cluster_size)? {
        return None;
    }
    Some((data_size, runs))
}

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
/// stream. A newly readable prefix can itself reveal another continuation,
/// so closure is iterative; duplicate descriptors are decoded once.
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
    let mut seen = FxHashSet::default();
    seen.insert((
        base_extent.target_reference,
        base_extent.starting_vcn,
        base_extent.id,
    ));
    let mut scanned_prefix = 0u64;
    while covered_prefix(&runs) < data_size {
        let prefix_len = covered_prefix(&runs).min(data_size);
        if prefix_len <= scanned_prefix {
            return None;
        }
        let entries = discover(&runs, prefix_len)?;
        let mut added_extent = false;
        for entry in entries {
            if !seen.insert((entry.target_reference, entry.starting_vcn, entry.id)) {
                continue;
            }
            let decoded = decode(entry)?;
            if decoded.is_empty() {
                return None;
            }
            runs.extend(decoded);
            added_extent = true;
        }
        scanned_prefix = prefix_len;
        if !added_extent {
            return None;
        }
    }
    Some(runs)
}

#[derive(Debug)]
pub enum ListStreamError {
    Io,
    Invalid,
    Cancelled,
}

impl From<std::io::Error> for ListStreamError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
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
        let mut runs = runs.to_vec();
        runs.sort_unstable_by_key(|run| run.logical);
        let mut covered = 0u64;
        for run in &runs {
            if run.logical != covered || run.len == 0 {
                return Err(ListStreamError::Invalid);
            }
            covered = covered
                .checked_add(run.len)
                .ok_or(ListStreamError::Invalid)?;
        }
        if covered < data_size {
            return Err(ListStreamError::Invalid);
        }
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

    fn discard(
        &mut self,
        mut len: u64,
        stop: &AtomicBool,
        scratch: &mut [u8],
    ) -> Result<(), ListStreamError> {
        while len > 0 {
            if stop.load(Ordering::Relaxed) {
                return Err(ListStreamError::Cancelled);
            }
            let take = usize::try_from(len.min(scratch.len() as u64))
                .map_err(|_| ListStreamError::Invalid)?;
            self.read_exact(&mut scratch[..take])?;
            len -= take as u64;
        }
        Ok(())
    }
}

/// Stream a non-resident list entry-by-entry with constant byte memory. Entry
/// lengths are `u16`, so only the fixed 26-byte header plus a 64KiB discard
/// buffer are ever resident regardless of the list's valid logical length.
///
/// `prefix` accepts an incomplete final entry when the caller is reading only
/// the base extent to discover continuation extents. The complete pass rejects
/// the same truncation.
pub fn visit_list_stream(
    reader: &mut (impl Read + Seek),
    runs: &[StreamRun],
    data_size: u64,
    stop: &AtomicBool,
    prefix: bool,
    mut visit: impl FnMut(ListEntry),
) -> Result<(), ListStreamError> {
    let header_len = size_of::<NtfsAttributeListEntry>();
    let mut stream = RunReader::new(reader, runs, data_size)?;
    let mut header = [0u8; size_of::<NtfsAttributeListEntry>()];
    let mut scratch = vec![0u8; STREAM_BUFFER_BYTES];
    while stream.remaining() > 0 {
        if stop.load(Ordering::Relaxed) {
            return Err(ListStreamError::Cancelled);
        }
        if stream.remaining() < header_len as u64 {
            let tail_len =
                usize::try_from(stream.remaining()).map_err(|_| ListStreamError::Invalid)?;
            let mut tail = [0u8; size_of::<NtfsAttributeListEntry>() - 1];
            stream.read_exact(&mut tail[..tail_len])?;
            if tail[..tail_len].iter().all(|&byte| byte == 0) || prefix {
                return Ok(());
            }
            return Err(ListStreamError::Invalid);
        }
        stream.read_exact(&mut header)?;
        let len = le_u16(&header, 4) as u64;
        if len < header_len as u64 {
            return Err(ListStreamError::Invalid);
        }
        let padded = len
            .checked_next_multiple_of(8)
            .ok_or(ListStreamError::Invalid)?;
        let after_header = padded
            .checked_sub(header_len as u64)
            .ok_or(ListStreamError::Invalid)?;
        if after_header > stream.remaining() {
            return if prefix {
                Ok(())
            } else {
                Err(ListStreamError::Invalid)
            };
        }
        let name_len = header[6] as usize;
        let name_off = header[7] as usize;
        let name_bytes = name_len.checked_mul(2).ok_or(ListStreamError::Invalid)?;
        if name_len > 0
            && (name_off < header_len
                || name_off
                    .checked_add(name_bytes)
                    .is_none_or(|end| end > len as usize))
        {
            return Err(ListStreamError::Invalid);
        }
        visit(ListEntry {
            type_id: le_u32(&header, 0),
            starting_vcn: le_u64(&header, 8),
            target_reference: le_u64(&header, 16),
            id: le_u16(&header, 24),
        });
        stream.discard(after_header, stop, &mut scratch)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntfs_reader::api::NtfsAttributeType;

    fn put_u16(data: &mut [u8], off: usize, value: u16) {
        data[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(data: &mut [u8], off: usize, value: u32) {
        data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(data: &mut [u8], off: usize, value: u64) {
        data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn list_entry(type_id: u32, vcn: u64, target: u64, id: u16) -> Vec<u8> {
        let len = size_of::<NtfsAttributeListEntry>();
        let record_len = len.next_multiple_of(8);
        let mut data = vec![0u8; record_len];
        put_u32(&mut data, 0, type_id);
        put_u16(&mut data, 4, record_len as u16);
        put_u64(&mut data, 8, vcn);
        put_u64(&mut data, 16, target);
        put_u16(&mut data, 24, id);
        data
    }

    fn nonresident_attr(lowest: u64, highest: u64, data_size: u64, mapping: &[u8]) -> Vec<u8> {
        let header = size_of::<NtfsNonResidentAttributeHeader>();
        let mut attr = vec![0u8; header + mapping.len()];
        put_u32(&mut attr, 0, NtfsAttributeType::AttributeList as u32);
        let attr_len = attr.len() as u32;
        put_u32(&mut attr, 4, attr_len);
        attr[8] = 1;
        put_u64(&mut attr, 16, lowest);
        put_u64(&mut attr, 24, highest);
        put_u16(&mut attr, 32, header as u16);
        put_u64(&mut attr, 48, data_size);
        attr[header..].copy_from_slice(mapping);
        attr
    }

    #[test]
    fn complete_and_prefix_entry_parsing_fail_closed() {
        let first = list_entry(0x20, 0, (3u64 << 48) | 0x2A, 7);
        let second = list_entry(0x30, 1, (4u64 << 48) | 0x63, 8);
        let mut all = first.clone();
        all.extend_from_slice(&second);
        let parsed = parse_list_entries(&all, false).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].target_record(), 42);
        assert_eq!(parsed[1].target_reference, (4u64 << 48) | 0x63);

        let cut = &all[..all.len() - 3];
        assert!(parse_list_entries(cut, false).is_none());
        assert_eq!(parse_list_entries(cut, true).unwrap().len(), 1);

        let mut bad_name = first;
        bad_name[6] = 2;
        bad_name[7] = 31;
        assert!(parse_list_entries(&bad_name, false).is_none());
    }

    #[test]
    fn run_decoder_handles_relative_sparse_and_negative_lcn_deltas() {
        // 2 clusters at LCN 10, one sparse cluster, one cluster at LCN 8
        // (delta -2 from the previous physical LCN).
        let mapping = [0x11, 2, 10, 0x01, 1, 0x11, 1, 0xFE, 0];
        let bytes = nonresident_attr(0, 3, 13, &mapping);
        let attr = NtfsAttribute::new(&bytes).unwrap();
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
                    physical: None,
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
    fn run_decoder_rejects_unterminated_mismatched_and_out_of_volume_runs() {
        let unterminated = nonresident_attr(0, 0, 4, &[0x11, 1, 1]);
        assert!(decode_extent_runs(&NtfsAttribute::new(&unterminated).unwrap(), 4, 1024).is_none());

        let wrong_vcn_span = nonresident_attr(0, 2, 4, &[0x11, 1, 1, 0]);
        assert!(
            decode_extent_runs(&NtfsAttribute::new(&wrong_vcn_span).unwrap(), 4, 1024).is_none()
        );

        let outside = nonresident_attr(0, 0, 4, &[0x11, 1, 0xFF, 0]);
        assert!(decode_extent_runs(&NtfsAttribute::new(&outside).unwrap(), 4, 64).is_none());
    }

    #[test]
    fn stream_parser_crosses_fragmented_runs_and_sparse_tail() {
        let first = list_entry(0x20, 0, (1u64 << 48) | 0x1E, 1);
        let second = list_entry(0x30, 1, (1u64 << 48) | 0x1F, 2);
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
                physical: None,
                len: 8,
            },
        ];
        let stop = AtomicBool::new(false);
        let mut got = Vec::new();
        visit_list_stream(&mut backing, &runs, 72, &stop, false, |entry| {
            got.push(entry);
        })
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].target_record(), 30);
        assert_eq!(got[1].target_record(), 31);

        stop.store(true, Ordering::Relaxed);
        assert!(matches!(
            visit_list_stream(&mut backing, &runs, 72, &stop, false, |_| {}),
            Err(ListStreamError::Cancelled)
        ));
    }

    #[test]
    fn stream_parser_has_no_legacy_four_megabyte_validity_cutoff() {
        let entry = list_entry(0x30, 0, (1u64 << 48) | 0x1E, 1);
        let count = ((4usize << 20) / entry.len()) + 2;
        let mut bytes = Vec::with_capacity(count * entry.len());
        for _ in 0..count {
            bytes.extend_from_slice(&entry);
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
    fn extent_closure_discovers_continuations_revealed_by_later_extents() {
        let base = ListEntry {
            type_id: NtfsAttributeType::AttributeList as u32,
            starting_vcn: 0,
            target_reference: 10,
            id: 1,
        };
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
        let base = ListEntry {
            type_id: NtfsAttributeType::AttributeList as u32,
            starting_vcn: 0,
            target_reference: 10,
            id: 1,
        };
        let initial = vec![StreamRun {
            logical: 0,
            physical: Some(100),
            len: 32,
        }];
        assert!(close_extent_runs(initial, 64, base, |_, _| Some(vec![base]), |_| None).is_none());
    }
}
