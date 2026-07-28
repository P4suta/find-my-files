#![no_main]
//! Fuzz the `$ATTRIBUTE_LIST` entry grammar, both from a flat buffer
//! (`parse_list_entries`, the resident case) and streamed over a volume
//! (`visit_list_stream`, the non-resident case). A file whose attributes spill
//! into extension records makes the scanner follow attacker-chosen references,
//! so this grammar decides how much work and memory a crafted volume can make
//! `fmf-service` do as `LocalSystem` (ADR-0047).
//!
//! Input shaping. One entry only starts decoding when its declared length is a
//! multiple of eight and at least 26, *and* its type code is a non-zero
//! multiple of 0x10 no greater than 0x100, *and* its target reference carries a
//! non-zero sequence value. Random bytes clear all three about once in
//! 2^27 entries, and a *sequence* of them never — which is a problem, because
//! the caps that actually bound `LocalSystem` (entry count, `$FILE_NAME`
//! fan-out, target/instance de-duplication, per-name monotonic VCN) only exist
//! across entries. `framed_entries` therefore stamps those three gates plus the
//! two shape rules that make an entry decodable at all (`$ATTRIBUTE_LIST` and
//! `$FILE_NAME` are unnamed; `$FILE_NAME` is not a split extent), and leaves
//! the target reference, the instance id, the starting VCN, the stream name and
//! the entry lengths to the fuzzer. Those are precisely the fields the caps
//! read. The raw input is still parsed alongside, so the fail-closed paths stay
//! under coverage.
//!
//! The streamed pass lays the same bytes on a synthetic volume as two runs with
//! the *second* half stored first, so `RunReader` must switch runs and seek
//! backwards to reassemble entries that straddle the boundary — the split point
//! is fuzzer-chosen, which is what makes an entry header land across it.

use std::io::Cursor;
use std::sync::atomic::AtomicBool;

use fmf_core::ondisk::attribute_list::{parse_list_entries, visit_list_stream, StreamRun};
use libfuzzer_sys::fuzz_target;

const ENTRY_HEADER_BYTES: usize = 26;
const ATTRIBUTE_LIST: u32 = 0x20;
const FILE_NAME: u32 = 0x30;
/// Type codes a real list carries, spanning the whole accepted range.
const TYPES: [u32; 6] = [0x10, 0x20, 0x30, 0x40, 0x80, 0x100];
/// Bound on the synthesized list so one input cannot dominate the run budget.
const MAX_FRAMED_BYTES: usize = 64 << 10;

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn le_u64(data: &[u8], offset: usize) -> u64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(out)
}

/// Turn `body` into a sequence of entries the decoder will admit, repairing
/// only the fields that gate entry and leaving the rest as fuzz bytes.
///
/// A short trailing chunk is emitted truncated on purpose: that is the exact
/// shape `prefix` mode exists to accept and a complete stream must reject.
fn framed_entries(body: &[u8]) -> Vec<u8> {
    let mut list: Vec<u8> = Vec::new();
    let mut cursor = 0usize;
    let mut previous_type = 0u32;
    let mut vcn_base = 0u64;

    while cursor < body.len() && list.len() < MAX_FRAMED_BYTES {
        let shape = body[cursor];
        // 32, 40, 48 or 56 bytes: a multiple of eight, at least the header.
        let length = ENTRY_HEADER_BYTES.next_multiple_of(8) + usize::from(shape & 3) * 8;
        let taken = (body.len() - cursor).min(length);
        let mut entry = vec![0u8; length];
        entry[..taken].copy_from_slice(&body[cursor..cursor + taken]);
        cursor += taken;

        // Type codes must not descend across the list; let the fuzzer pick from
        // the valid set and lift the choice to the running maximum rather than
        // discarding every input whose second entry sorts low.
        let type_id = TYPES[usize::from(shape >> 4) % TYPES.len()].max(previous_type);
        previous_type = type_id;
        put_u32(&mut entry, 0, type_id);
        put_u16(&mut entry, 4, length as u16);

        // Stream names are legal for every type except the two the scanner
        // follows, and must start immediately after the header.
        let name_units = if type_id == ATTRIBUTE_LIST || type_id == FILE_NAME {
            0
        } else {
            usize::from(entry[6]) % ((length - ENTRY_HEADER_BYTES) / 2 + 1)
        };
        entry[6] = name_units as u8;
        entry[7] = if name_units == 0 {
            0
        } else {
            ENTRY_HEADER_BYTES as u8
        };
        // Entries of one attribute must carry strictly increasing VCNs, which a
        // random 64-bit field satisfies for a *pair* half the time and for a
        // whole list essentially never: leaving this raw had under 1% of framed
        // lists decode past their second entry, so the cross-entry caps (entry
        // count, `$FILE_NAME` fan-out, target/instance de-duplication) — the
        // rules that actually bound LocalSystem — were unreachable. Walking a
        // base forward by four with a 0..=7 jitter keeps lists mostly ordered
        // while still letting the fuzzer drive a decrease.
        let jitter = u64::from(entry[9] & 7);
        vcn_base += 4;
        put_u64(&mut entry, 8, vcn_base + jitter);
        if type_id == FILE_NAME {
            entry[8..16].fill(0); // resident-only: never a split extent
        }
        if le_u64(&entry, 16) >> 48 == 0 {
            entry[23] |= 1; // a target reference needs a sequence value
        }

        entry.truncate(taken);
        list.extend_from_slice(&entry);
    }
    list
}

fuzz_target!(|data: &[u8]| {
    let control = |index: usize| data.get(index).copied().unwrap_or(0);
    let body = data.get(2..).unwrap_or_default();

    // Raw bytes, both completeness modes: the fail-closed paths.
    for prefix in [false, true] {
        let _ = parse_list_entries(data, prefix);
    }

    let list = framed_entries(body);
    for prefix in [false, true] {
        let _ = parse_list_entries(&list, prefix);
    }
    if list.is_empty() {
        return;
    }

    // Lay the list on a synthetic volume as two runs, storing the second half
    // first so reassembly has to seek backwards across the run boundary.
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
        let _ = visit_list_stream(&mut cursor, &runs, data_size, &stop, prefix, |_| {});
    }

    // The cancellation path: a stop flag already set must unwind cleanly rather
    // than leave the reader mid-entry.
    let cancelled = AtomicBool::new(true);
    let _ = visit_list_stream(&mut cursor, &runs, data_size, &cancelled, false, |_| {});
});
