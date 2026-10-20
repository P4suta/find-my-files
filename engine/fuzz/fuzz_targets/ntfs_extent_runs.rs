#![no_main]
//! Check decoded run bounds and bounded continuation discovery on raw and shaped mapping pairs.

use fmf_core::ondisk::ntfs::{NtfsAttribute, NtfsAttributeType};
use fmf_runtime::ondisk::attribute_list::{
    close_extent_runs, covered_prefix, decode_extent_runs, ListEntry, StreamRun,
};
use libfuzzer_sys::fuzz_target;

const ATTRIBUTE_ALIGNMENT: usize = 8;
const LENGTH_OFFSET: usize = 4;
const RESIDENCY_OFFSET: usize = 8;
const NAME_UNITS_OFFSET: usize = 9;
const NAME_OFFSET_FIELD: usize = 10;
const FLAGS_OFFSET: usize = 12;
const LOWEST_VCN_OFFSET: usize = 16;
const HIGHEST_VCN_OFFSET: usize = 24;
const MAPPING_OFFSET: usize = 32;
const RESERVED_RANGE: core::ops::Range<usize> = 34..40;
const ALLOCATED_SIZE_OFFSET: usize = 40;
const DATA_SIZE_OFFSET: usize = 48;
const INITIALIZED_SIZE_OFFSET: usize = 56;
const MIN_CLUSTER_SHIFT: u32 = 9;
const CLUSTER_SHIFT_CHOICES: u64 = 4;
const MIN_VOLUME_SHIFT: u32 = 20;
const VOLUME_SHIFT_CHOICES: u64 = 24;
const CLUSTERS_CONTROL: usize = 2;
const TYPE_CONTROL: usize = 3;
const DATA_SIZE_CONTROL: usize = 4;
const STREAM_SIZE_CONTROL: usize = 5;
const BYTE_CHOICES: u64 = u8::MAX as u64 + 1;
const ONE_BYTE_MAPPING_DESCRIPTOR: u8 = 0x11;
const SECTOR_BYTES: u64 = 512;
const STREAM_SECTOR_CHOICES: u64 = 128;
const INITIAL_SECTOR_CHOICES: u64 = 8;
const ROOT_REFERENCE: u64 = (1 << 48) | 5;
const FIRST_CONTINUATION_ID: u16 = 2;
const SEQUENCE_SHIFT: u32 = u64::BITS - u16::BITS;
const MAX_CLOSURE_PASSES: usize = 64;
const MAX_CLOSURE_SCAN_BYTES: u64 = 64 << 20;

const NONRESIDENT_HEADER_BYTES: usize = 64;
const CONTROL_BYTES: usize = 6;
/// Clusters the framed extent spans; small enough that the fuzzer can make the mapping pairs sum to it, which is the loop's final equality check.
const MAX_EXTENT_CLUSTERS: u64 = 16;

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + size_of::<u16>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

/// A first-extent non-resident attribute whose header clears every gate and whose mapping pairs are `mapping`.
fn framed_extent(
    type_id: u32,
    clusters: u64,
    cluster_size: u64,
    data_size: u64,
    mapping: &[u8],
) -> Vec<u8> {
    let length =
        (NONRESIDENT_HEADER_BYTES + mapping.len().max(1)).next_multiple_of(ATTRIBUTE_ALIGNMENT);
    let mut attribute = vec![0u8; length];
    let copied = mapping.len().min(length - NONRESIDENT_HEADER_BYTES);
    attribute[NONRESIDENT_HEADER_BYTES..NONRESIDENT_HEADER_BYTES + copied]
        .copy_from_slice(&mapping[..copied]);

    put_u32(&mut attribute, 0, type_id);
    put_u32(&mut attribute, LENGTH_OFFSET, length as u32);
    attribute[RESIDENCY_OFFSET] = 1;
    attribute[NAME_UNITS_OFFSET] = 0;
    put_u16(&mut attribute, NAME_OFFSET_FIELD, 0);
    put_u16(&mut attribute, FLAGS_OFFSET, 0);
    put_u64(&mut attribute, LOWEST_VCN_OFFSET, 0);
    put_u64(&mut attribute, HIGHEST_VCN_OFFSET, clusters - 1);
    put_u16(
        &mut attribute,
        MAPPING_OFFSET,
        NONRESIDENT_HEADER_BYTES as u16,
    );
    attribute[RESERVED_RANGE].fill(0);
    let allocated = clusters * cluster_size;
    put_u64(&mut attribute, ALLOCATED_SIZE_OFFSET, allocated);
    put_u64(&mut attribute, DATA_SIZE_OFFSET, data_size);
    put_u64(&mut attribute, INITIALIZED_SIZE_OFFSET, data_size);
    attribute
}

fn check_runs(bytes: &[u8], cluster_size: u64, volume_size: u64) {
    if let Some(attribute) = NtfsAttribute::parse(bytes) {
        if let Some((_, runs)) = decode_extent_runs(&attribute, cluster_size, volume_size) {
            let mut total = 0u64;
            for run in &runs {
                assert!(run.len > 0);
                assert!(run.len.is_multiple_of(cluster_size));
                let physical = run.physical.expect("decoded extents are not sparse");
                assert!(physical > 0);
                assert!(physical
                    .checked_add(run.len)
                    .is_some_and(|end| end <= volume_size));
                total = total
                    .checked_add(run.len)
                    .expect("decoded extent length fits the volume");
            }
            assert!(covered_prefix(&runs) <= total);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let control = |index: usize| u64::from(data.get(index).copied().unwrap_or(0));
    let mapping = data.get(CONTROL_BYTES..).unwrap_or_default();

    let cluster_size = 1u64 << (u64::from(MIN_CLUSTER_SHIFT) + control(0) % CLUSTER_SHIFT_CHOICES);
    let volume_size = 1u64 << (u64::from(MIN_VOLUME_SHIFT) + control(1) % VOLUME_SHIFT_CHOICES);
    check_runs(data, cluster_size, volume_size);

    let clusters = 1 + control(CLUSTERS_CONTROL) % MAX_EXTENT_CLUSTERS;
    let type_id = if control(TYPE_CONTROL) & 1 == 0 {
        NtfsAttributeType::AttributeList as u32
    } else {
        NtfsAttributeType::Data as u32
    };
    let allocated = clusters * cluster_size;
    let data_size = 1 + control(DATA_SIZE_CONTROL) * allocated / BYTE_CHOICES;
    let framed = framed_extent(
        type_id,
        clusters,
        cluster_size,
        data_size.min(allocated),
        mapping,
    );
    check_runs(&framed, cluster_size, volume_size);

    let mut seeded = vec![ONE_BYTE_MAPPING_DESCRIPTOR, clusters as u8];
    seeded.extend_from_slice(mapping);
    let framed = framed_extent(
        type_id,
        clusters,
        cluster_size,
        data_size.min(allocated),
        &seeded,
    );
    check_runs(&framed, cluster_size, volume_size);

    let stream_size = SECTOR_BYTES * (1 + control(STREAM_SIZE_CONTROL) % STREAM_SECTOR_CHOICES);
    let first_len = SECTOR_BYTES * (1 + control(0) % INITIAL_SECTOR_CHOICES);
    let initial = vec![StreamRun {
        logical: 0,
        physical: Some(cluster_size),
        len: first_len,
    }];
    let base = ListEntry::unnamed(
        NtfsAttributeType::AttributeList as u32,
        0,
        ROOT_REFERENCE,
        1,
    );
    let mut next_id = FIRST_CONTINUATION_ID;
    let mut logical = first_len;
    let mut physical = cluster_size + first_len;
    let mut step = CONTROL_BYTES;
    let mut passes = 0;
    let mut scanned = 0u64;
    let result = close_extent_runs(
        initial,
        stream_size,
        base,
        |_, prefix| {
            passes += 1;
            scanned = scanned.checked_add(prefix).expect("bounded discovery scan");
            let entry = ListEntry::unnamed(
                NtfsAttributeType::AttributeList as u32,
                0,
                (1 << SEQUENCE_SHIFT) | u64::from(next_id),
                next_id,
            );
            next_id = next_id.wrapping_add(1);
            Some(vec![entry])
        },
        |_| {
            let len = SECTOR_BYTES * (1 + u64::from(data.get(step).copied().unwrap_or(0)));
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
    assert!(passes <= MAX_CLOSURE_PASSES);
    assert!(scanned <= MAX_CLOSURE_SCAN_BYTES);
    if let Some(runs) = result {
        assert!(covered_prefix(&runs) >= stream_size);
    }
});
