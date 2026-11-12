#![no_main]
//! Compare flat and streamed list decoding across reversed physical runs and cancellation.

use std::io::Cursor;
use std::sync::atomic::AtomicBool;

use fmf_runtime::ondisk::attribute_list::{
    parse_list_entries, visit_list_stream, ListEntry, ListStreamError, StreamRun,
};
use libfuzzer_sys::fuzz_target;

const ATTRIBUTE_ALIGNMENT: usize = 8;
const LENGTH_OFFSET: usize = 4;
const NAME_UNITS_OFFSET: usize = 6;
const NAME_OFFSET_FIELD: usize = 7;
const VCN_OFFSET: usize = 8;
const VCN_JITTER_OFFSET: usize = 9;
const TARGET_OFFSET: usize = 16;
const TARGET_HIGH_BYTE: usize = 23;
const SEQUENCE_SHIFT: u32 = u64::BITS - u16::BITS;
const LENGTH_SHAPE_MASK: u8 = 3;
const TYPE_SHAPE_SHIFT: u32 = 4;
const VCN_JITTER_MASK: u8 = 7;
const VCN_STEP: u64 = 4;
const CONTROL_BYTES: usize = 2;
const MAX_LIST_ENTRIES: usize = 262_144;

const ENTRY_HEADER_BYTES: usize = 26;
const ATTRIBUTE_LIST: u32 = 0x20;
const FILE_NAME: u32 = 0x30;
/// Type codes a real list carries, spanning the whole accepted range.
const TYPES: [u32; 6] = [0x10, 0x20, 0x30, 0x40, 0x80, 0x100];
/// Bound on the synthesized list so one input cannot dominate the run budget.
const MAX_FRAMED_BYTES: usize = 64 << 10;

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + size_of::<u16>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

fn le_u64(data: &[u8], offset: usize) -> u64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&data[offset..offset + size_of::<u64>()]);
    u64::from_le_bytes(out)
}

/// Turn `body` into a sequence of entries the decoder will admit, repairing only the fields that gate entry and leaving the rest as fuzz bytes.
///
/// A short trailing chunk is emitted truncated on purpose: that is the exact shape `prefix` mode exists to accept and a complete stream must reject.
fn framed_entries(body: &[u8]) -> Vec<u8> {
    let mut list: Vec<u8> = Vec::new();
    let mut cursor = 0usize;
    let mut previous_type = 0u32;
    let mut vcn_base = 0u64;

    while cursor < body.len() && list.len() < MAX_FRAMED_BYTES {
        let shape = body[cursor];
        let length = ENTRY_HEADER_BYTES.next_multiple_of(ATTRIBUTE_ALIGNMENT)
            + usize::from(shape & LENGTH_SHAPE_MASK) * ATTRIBUTE_ALIGNMENT;
        let taken = (body.len() - cursor).min(length);
        let mut entry = vec![0u8; length];
        entry[..taken].copy_from_slice(&body[cursor..cursor + taken]);
        cursor += taken;

        let type_id =
            TYPES[usize::from(shape >> TYPE_SHAPE_SHIFT) % TYPES.len()].max(previous_type);
        previous_type = type_id;
        put_u32(&mut entry, 0, type_id);
        put_u16(&mut entry, LENGTH_OFFSET, length as u16);

        let name_units = if type_id == ATTRIBUTE_LIST || type_id == FILE_NAME {
            0
        } else {
            usize::from(entry[NAME_UNITS_OFFSET])
                % ((length - ENTRY_HEADER_BYTES) / size_of::<u16>() + 1)
        };
        entry[NAME_UNITS_OFFSET] = name_units as u8;
        entry[NAME_OFFSET_FIELD] = if name_units == 0 {
            0
        } else {
            ENTRY_HEADER_BYTES as u8
        };
        let jitter = u64::from(entry[VCN_JITTER_OFFSET] & VCN_JITTER_MASK);
        vcn_base += VCN_STEP;
        put_u64(&mut entry, VCN_OFFSET, vcn_base + jitter);
        if type_id == FILE_NAME {
            entry[VCN_OFFSET..TARGET_OFFSET].fill(0);
        }
        if le_u64(&entry, TARGET_OFFSET) >> SEQUENCE_SHIFT == 0 {
            entry[TARGET_HIGH_BYTE] |= 1;
        }

        entry.truncate(taken);
        list.extend_from_slice(&entry);
    }
    list
}

fn check_entries(entries: &[ListEntry]) {
    assert!(entries.len() <= MAX_LIST_ENTRIES);
    let mut targets = std::collections::HashSet::new();
    let mut previous_type = 0;
    for entry in entries {
        assert!(entry.type_id >= previous_type);
        assert_ne!(entry.target_reference >> SEQUENCE_SHIFT, 0);
        assert!(targets.insert((entry.target_reference, entry.id)));
        previous_type = entry.type_id;
    }
}

fuzz_target!(|data: &[u8]| {
    let control = |index: usize| data.get(index).copied().unwrap_or(0);
    let body = data.get(CONTROL_BYTES..).unwrap_or_default();

    for prefix in [false, true] {
        if let Some(entries) = parse_list_entries(data, prefix) {
            check_entries(&entries);
        }
    }

    let list = framed_entries(body);
    for prefix in [false, true] {
        if let Some(entries) = parse_list_entries(&list, prefix) {
            check_entries(&entries);
        }
    }
    if list.is_empty() {
        return;
    }

    let data_size = list.len() as u64;
    let split = 1 + usize::from(control(1)) % list.len();
    let (head, tail) = list.split_at(split);
    let mut volume = Vec::with_capacity(list.len());
    volume.extend_from_slice(tail);
    volume.extend_from_slice(head);
    let mut runs = vec![StreamRun {
        logical: 0,
        physical: Some(tail.len() as u64),
        len: head.len() as u64,
    }];
    if !tail.is_empty() {
        runs.push(StreamRun {
            logical: head.len() as u64,
            physical: Some(0),
            len: tail.len() as u64,
        });
    }

    let mut cursor = Cursor::new(volume);
    let stop = AtomicBool::new(false);
    for prefix in [false, true] {
        let flat = parse_list_entries(&list, prefix);
        let mut streamed = Vec::new();
        let result = visit_list_stream(&mut cursor, &runs, data_size, &stop, prefix, |entry| {
            streamed.push(entry)
        });
        assert_eq!(result.is_ok(), flat.is_some());
        if let Some(entries) = flat {
            assert_eq!(streamed, entries);
        }
    }

    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        visit_list_stream(
            &mut cursor,
            &runs,
            data_size,
            &cancelled,
            false,
            |_| panic!("cancelled stream visited an entry")
        ),
        Err(ListStreamError::Cancelled)
    ));
});
