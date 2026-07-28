#![no_main]
//! Fuzz the non-resident run arithmetic: NTFS mapping-pair decoding
//! (`decode_extent_runs`) and the continuation closure that chases an
//! `$ATTRIBUTE_LIST` split across extension records (`close_extent_runs`).
//!
//! This is where untrusted disk bytes become *volume byte offsets the scanner
//! then reads from*, so every product and sum here is a bounds decision made on
//! behalf of a `LocalSystem` process (ADR-0047). It is split out of
//! `ntfs_attribute_list` because reaching it needs an entirely different input
//! shape, and a shared target would spend its budget on whichever gate is
//! cheaper rather than on both grammars.
//!
//! Input shaping. Mapping pairs are only read once the attribute clears a long
//! conjunction: unnamed, unflagged, `$ATTRIBUTE_LIST` or `$DATA`, length a
//! multiple of eight, mapping-pair offset exactly 64, six reserved bytes zero,
//! and — for the first extent — an allocated size that is a non-zero multiple
//! of the cluster size, at least as large as the extent and the data size, and
//! within the volume. `framed_extent` satisfies exactly that conjunction from a
//! four-byte control prefix and hands the *entire* mapping-pair region to the
//! fuzzer, so the run loop's real grammar — the count/offset nibble widths, the
//! sign-extended LCN deltas accumulated in `i128`, the cluster multiplications,
//! the volume containment and disjointness checks — is what gets mutated. The
//! raw input is decoded as an attribute too, keeping the rejection paths live.
//!
//! `close_extent_runs` is driven with closures that always extend coverage by a
//! fuzz-chosen amount and always hand back a fresh target/instance pair. That
//! is the adversarial shape: a volume that keeps promising one more extent. The
//! target proves the pass and scanned-byte ceilings stop it.

use fmf_core::ondisk::attribute_list::{
    close_extent_runs, covered_prefix, decode_extent_runs, ListEntry, StreamRun,
};
use fmf_core::ondisk::ntfs::{NtfsAttribute, NtfsAttributeType};
use libfuzzer_sys::fuzz_target;

const NONRESIDENT_HEADER_BYTES: usize = 64;
const CONTROL_BYTES: usize = 6;
/// Clusters the framed extent spans; small enough that the fuzzer can make the
/// mapping pairs sum to it, which is the loop's final equality check.
const MAX_EXTENT_CLUSTERS: u64 = 16;

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// A first-extent non-resident attribute whose header clears every gate and
/// whose mapping pairs are `mapping`.
fn framed_extent(
    type_id: u32,
    clusters: u64,
    cluster_size: u64,
    data_size: u64,
    mapping: &[u8],
) -> Vec<u8> {
    let length = (NONRESIDENT_HEADER_BYTES + mapping.len().max(1)).next_multiple_of(8);
    let mut attribute = vec![0u8; length];
    let copied = mapping.len().min(length - NONRESIDENT_HEADER_BYTES);
    attribute[NONRESIDENT_HEADER_BYTES..NONRESIDENT_HEADER_BYTES + copied]
        .copy_from_slice(&mapping[..copied]);

    put_u32(&mut attribute, 0, type_id);
    put_u32(&mut attribute, 4, length as u32);
    attribute[8] = 1; // non-resident
    attribute[9] = 0; // unnamed
    put_u16(&mut attribute, 10, 0);
    put_u16(&mut attribute, 12, 0); // neither compressed, sparse nor encrypted
    put_u64(&mut attribute, 16, 0); // first extent: lowest VCN is zero
    put_u64(&mut attribute, 24, clusters - 1);
    put_u16(&mut attribute, 32, NONRESIDENT_HEADER_BYTES as u16);
    attribute[34..40].fill(0); // reserved for an ordinary allocated stream
    let allocated = clusters * cluster_size;
    put_u64(&mut attribute, 40, allocated);
    put_u64(&mut attribute, 48, data_size);
    put_u64(&mut attribute, 56, data_size); // initialized size
    attribute
}

fuzz_target!(|data: &[u8]| {
    let control = |index: usize| u64::from(data.get(index).copied().unwrap_or(0));
    let mapping = data.get(CONTROL_BYTES..).unwrap_or_default();

    // Raw bytes as an attribute, with a fuzz-chosen but valid geometry: keeps
    // the rejections that happen before any run is decoded under coverage.
    let cluster_size = 1u64 << (9 + control(0) % 4); // 512..=4096
    let volume_size = 1u64 << (20 + control(1) % 24); // 1MiB..=8TiB
    if let Some(attribute) = NtfsAttribute::parse(data) {
        if let Some((_, runs)) = decode_extent_runs(&attribute, cluster_size, volume_size) {
            let _ = covered_prefix(&runs);
        }
    }

    // Past the header conjunction: the mapping-pair grammar itself.
    let clusters = 1 + control(2) % MAX_EXTENT_CLUSTERS;
    let type_id = if control(3) % 2 == 0 {
        NtfsAttributeType::AttributeList as u32
    } else {
        NtfsAttributeType::Data as u32
    };
    let allocated = clusters * cluster_size;
    let data_size = 1 + control(4) * allocated / 256;
    let framed = framed_extent(
        type_id,
        clusters,
        cluster_size,
        data_size.min(allocated),
        mapping,
    );
    if let Some(attribute) = NtfsAttribute::parse(&framed) {
        if let Some((size, runs)) = decode_extent_runs(&attribute, cluster_size, volume_size) {
            let _ = covered_prefix(&runs);
            let _ = size;
        }
    }

    // The run loop ends on an exact equality: the decoded runs must span the
    // extent's VCN range to the cluster. Random mapping pairs never satisfy it
    // (measured at 0 in 200k inputs), which leaves the disjointness check, the
    // multi-run accumulation and `covered_prefix` behind it unreachable. Seed
    // one leading run that already spans the extent — its LCN and everything
    // after it stay fuzzed, so the fuzzer decides whether the mapping stops
    // there, adds a run that breaks the equality, or overlaps an earlier one.
    let mut seeded = vec![0x11, clusters as u8];
    seeded.extend_from_slice(mapping);
    let framed = framed_extent(
        type_id,
        clusters,
        cluster_size,
        data_size.min(allocated),
        &seeded,
    );
    if let Some(attribute) = NtfsAttribute::parse(&framed) {
        if let Some((_, runs)) = decode_extent_runs(&attribute, cluster_size, volume_size) {
            let _ = covered_prefix(&runs);
        }
    }

    // A stream that keeps promising one more continuation extent.
    let stream_size = 512 * (1 + control(5) % 128);
    let first_len = 512 * (1 + control(0) % 8);
    let initial = vec![StreamRun {
        logical: 0,
        physical: Some(cluster_size),
        len: first_len,
    }];
    let base = ListEntry::unnamed(NtfsAttributeType::AttributeList as u32, 0, (1 << 48) | 5, 1);
    let mut next_id = 2u16;
    let mut logical = first_len;
    let mut physical = cluster_size + first_len;
    let mut step = CONTROL_BYTES;
    let _ = close_extent_runs(
        initial,
        stream_size,
        base,
        |_, _| {
            // Always a target/instance pair never seen before, so the closure
            // never terminates on the de-duplication rule: only the pass and
            // scanned-byte ceilings can stop it.
            let entry = ListEntry::unnamed(
                NtfsAttributeType::AttributeList as u32,
                0,
                (1 << 48) | u64::from(next_id),
                next_id,
            );
            next_id = next_id.wrapping_add(1);
            Some(vec![entry])
        },
        |_| {
            let len = 512 * (1 + u64::from(data.get(step).copied().unwrap_or(0)));
            step += 1;
            let run = StreamRun {
                logical,
                physical: Some(physical),
                len,
            };
            logical = logical.checked_add(len)?;
            physical = physical.checked_add(len)?;
            Some(vec![run])
        },
    );
});
