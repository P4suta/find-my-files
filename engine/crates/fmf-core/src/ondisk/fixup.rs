//! The NTFS update-sequence array (fixup) applied to one record buffer.
//!
//! Every $MFT / $LogFile-style record has its last two bytes per sector replaced by a sentinel before it is written, and the displaced bytes are parked in an update-sequence array in the record header.
//! Undoing that is pure byte arithmetic over untrusted disk bytes, so it lives here next to the rest of the on-disk grammar rather than beside the Windows volume handles that fetch the buffer.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

const USA_OFFSET_FIELD: usize = 4;
const USA_LENGTH_FIELD: usize = 6;
const ATTRIBUTES_OFFSET_FIELD: usize = 20;

const SECTOR: usize = 512;
const MAX_SECTOR: usize = 4096;
const FILE_HEADER_BYTES: usize = 42;
const ATTRIBUTE_OFFSET_END: usize = 22;
const UPDATE_SEQUENCE_WORD_BYTES: usize = size_of::<u16>();

/// Apply the NTFS update sequence array in place.
/// Returns false when the sector check bytes don't match (torn/corrupt record).
#[must_use]
pub fn apply_fixup(data: &mut [u8], sector_size: usize) -> bool {
    let Some((uso, usl)) = fixup_layout(data, sector_size) else {
        return false;
    };
    let usn = [data[uso], data[uso + 1]];
    let fixups: Vec<[u8; 2]> = (1..usl)
        .map(|i| {
            let usa_off = uso + i * UPDATE_SEQUENCE_WORD_BYTES;
            [data[usa_off], data[usa_off + 1]]
        })
        .collect();

    /* INVARIANT: Validate every sector before changing the record; snapshot the update-sequence array before any overlapping sector-tail write. */
    for sector in 1..usl {
        let sector_off = sector * sector_size - UPDATE_SEQUENCE_WORD_BYTES;
        if data[sector_off..sector_off + UPDATE_SEQUENCE_WORD_BYTES] != usn {
            return false;
        }
    }
    for (sector, fixup) in fixups.into_iter().enumerate() {
        let sector_off = (sector + 1) * sector_size - UPDATE_SEQUENCE_WORD_BYTES;
        data[sector_off..sector_off + UPDATE_SEQUENCE_WORD_BYTES].copy_from_slice(&fixup);
    }
    true
}

/// Checked update-sequence offset and word count for one record.
#[must_use]
pub fn fixup_layout(data: &[u8], sector_size: usize) -> Option<(usize, usize)> {
    if data.len() < ATTRIBUTE_OFFSET_END {
        return None;
    }
    checked_fixup_layout(
        data.len(),
        sector_size,
        u16::from_le_bytes([data[USA_OFFSET_FIELD], data[USA_OFFSET_FIELD + 1]]),
        u16::from_le_bytes([data[USA_LENGTH_FIELD], data[USA_LENGTH_FIELD + 1]]),
        u16::from_le_bytes([
            data[ATTRIBUTES_OFFSET_FIELD],
            data[ATTRIBUTES_OFFSET_FIELD + 1],
        ]),
    )
}

fn checked_fixup_layout(
    record_bytes: usize,
    sector_size: usize,
    update_sequence_offset: u16,
    update_sequence_length: u16,
    attributes_offset: u16,
) -> Option<(usize, usize)> {
    if record_bytes < sector_size
        || !(SECTOR..=MAX_SECTOR).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || !record_bytes.is_multiple_of(sector_size)
    {
        return None;
    }
    let uso = usize::from(update_sequence_offset);
    let usl = usize::from(update_sequence_length);
    let expected_usl = record_bytes.checked_div(sector_size)?.checked_add(1)?;
    let usa_bytes = usl.checked_mul(UPDATE_SEQUENCE_WORD_BYTES)?;
    let usa_end = uso.checked_add(usa_bytes)?;
    let attributes_offset = usize::from(attributes_offset);
    if uso < FILE_HEADER_BYTES
        || !uso.is_multiple_of(UPDATE_SEQUENCE_WORD_BYTES)
        || usl != expected_usl
        || usa_end > record_bytes
        || usa_end > attributes_offset
        || attributes_offset >= record_bytes
    {
        return None;
    }
    Some((uso, usl))
}

#[cfg(kani)]
#[path = "fixup_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `len`-byte record with an update-sequence array at `uso` carrying `usn` plus `fixups` (the bytes that belong at each sector tail), and write the `usn` sentinel into each sector tail so a correct `apply_fixup` succeeds and restores the `fixups`.
    /// The record spans one `len / fixups.len()` sector per fixup.
    fn record_with_usa(len: usize, uso: usize, usn: u16, fixups: &[u16]) -> Vec<u8> {
        record_with_usa_of(len, len / fixups.len(), uso, usn, fixups)
    }

    /// `record_with_usa` for a record whose sector size is not simply its length divided by the number of fixups: the sector tails land at `sector_size` strides, so the record can be given a geometry that leaves a partial sector at the end or that no volume could be formatted with.
    fn record_with_usa_of(
        len: usize,
        sector_size: usize,
        uso: usize,
        usn: u16,
        fixups: &[u16],
    ) -> Vec<u8> {
        let mut r = vec![0u8; len];
        let usl = (fixups.len() + 1) as u16;
        let attributes_offset = (uso + usize::from(usl) * 2).next_multiple_of(8);
        r[4..6].copy_from_slice(&(uso as u16).to_le_bytes());
        r[6..8].copy_from_slice(&usl.to_le_bytes());
        r[20..22].copy_from_slice(&(attributes_offset as u16).to_le_bytes());
        r[uso..uso + 2].copy_from_slice(&usn.to_le_bytes());
        for (i, f) in fixups.iter().enumerate() {
            let off = uso + (i + 1) * 2;
            r[off..off + 2].copy_from_slice(&f.to_le_bytes());
            let tail = (i + 1) * sector_size - 2;
            r[tail..tail + 2].copy_from_slice(&usn.to_le_bytes());
        }
        r
    }

    #[test]
    fn rejects_a_buffer_too_small_for_a_header() {
        assert!(!apply_fixup(&mut [0u8; 47], SECTOR));
    }

    #[test]
    fn rejects_an_update_sequence_length_below_two() {
        let mut r = vec![0u8; 1024];
        r[4..6].copy_from_slice(&48u16.to_le_bytes());
        r[6..8].copy_from_slice(&1u16.to_le_bytes());
        assert!(!apply_fixup(&mut r, SECTOR));
    }

    #[test]
    fn rejects_a_usa_that_does_not_cover_every_sector_exactly() {
        let mut too_few = record_with_usa(1024, 48, 0x0001, &[0xAAAA]);
        assert!(!apply_fixup(&mut too_few, SECTOR));

        let mut too_many = vec![0u8; 1024];
        too_many[4..6].copy_from_slice(&48u16.to_le_bytes());
        too_many[6..8].copy_from_slice(&4u16.to_le_bytes());
        assert!(!apply_fixup(&mut too_many, SECTOR));
    }

    #[test]
    fn rejects_a_usa_that_runs_past_the_buffer() {
        let mut r = vec![0u8; 1024];
        r[4..6].copy_from_slice(&1020u16.to_le_bytes());
        r[6..8].copy_from_slice(&8u16.to_le_bytes());
        assert!(!apply_fixup(&mut r, SECTOR));
    }

    #[test]
    fn rejects_a_misaligned_or_header_overlapping_usa() {
        for offset in [6u16, 8, 16, 40] {
            let mut overlaps_header = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
            overlaps_header[4..6].copy_from_slice(&offset.to_le_bytes());
            assert!(!apply_fixup(&mut overlaps_header, SECTOR));
        }

        let mut misaligned = record_with_usa(1024, 49, 0x0001, &[0xAAAA, 0xBBBB]);
        assert!(!apply_fixup(&mut misaligned, SECTOR));
    }

    #[test]
    fn applies_the_update_sequence_and_restores_sector_tails() {
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        assert!(apply_fixup(&mut r, SECTOR));
        assert_eq!(u16::from_le_bytes([r[510], r[511]]), 0xAAAA);
        assert_eq!(u16::from_le_bytes([r[1022], r[1023]]), 0xBBBB);
    }

    #[test]
    fn applies_a_4kn_update_sequence_only_with_the_boot_sector_size() {
        let mut record = record_with_usa(4096, 48, 0x1234, &[0xBEEF]);
        let mut wrong_geometry = record.clone();
        assert!(!apply_fixup(&mut wrong_geometry, SECTOR));
        assert!(apply_fixup(&mut record, 4096));
        assert_eq!(u16::from_le_bytes([record[4094], record[4095]]), 0xBEEF);
    }

    #[test]
    fn rejects_a_torn_record_whose_sector_tail_lost_the_sentinel() {
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        r[1022] = 0x99;
        assert!(!apply_fixup(&mut r, SECTOR));
    }

    #[test]
    fn rejects_a_record_with_no_room_for_even_one_sector() {
        let mut empty: [u8; 0] = [];
        assert!(!apply_fixup(&mut empty, SECTOR));
    }

    #[test]
    fn rejects_a_sector_size_outside_the_range_a_volume_can_be_formatted_with() {
        let mut below_range =
            record_with_usa_of(1024, 256, 48, 0x0001, &[0xAAAA, 0xBBBB, 0xCCCC, 0xDDDD]);
        assert!(!apply_fixup(&mut below_range, 256));

        let mut above_range = record_with_usa_of(8192, 8192, 48, 0x0001, &[0xBEEF]);
        assert!(!apply_fixup(&mut above_range, 8192));
    }

    #[test]
    fn rejects_a_sector_size_that_is_not_a_power_of_two() {
        let mut r = record_with_usa_of(3072, 1536, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        assert!(!apply_fixup(&mut r, 1536));
    }

    #[test]
    fn rejects_a_record_truncated_part_way_through_its_last_sector() {
        let mut r = record_with_usa_of(1792, SECTOR, 48, 0x0001, &[0xAAAA, 0xBBBB, 0xCCCC]);
        assert!(!apply_fixup(&mut r, SECTOR));
    }

    #[test]
    fn rejects_an_update_sequence_array_that_starts_inside_the_record_header() {
        let mut r = record_with_usa(1024, 40, 0x0001, &[0xAAAA, 0xBBBB]);
        assert!(!apply_fixup(&mut r, SECTOR));
    }

    #[test]
    fn accepts_an_update_sequence_array_at_the_first_offset_past_the_header() {
        let mut r = record_with_usa(1024, 42, 0x0001, &[0xAAAA, 0xBBBB]);
        assert_eq!(fixup_layout(&r, SECTOR), Some((42, 3)));
        assert!(apply_fixup(&mut r, SECTOR));
        assert_eq!(u16::from_le_bytes([r[510], r[511]]), 0xAAAA);
        assert_eq!(u16::from_le_bytes([r[1022], r[1023]]), 0xBBBB);
    }

    #[test]
    fn rejects_a_usa_that_leaves_the_last_sector_unverified() {
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        r[6..8].copy_from_slice(&2u16.to_le_bytes());
        let pristine = r.clone();
        assert!(!apply_fixup(&mut r, SECTOR));
        assert_eq!(r, pristine);
    }

    #[test]
    fn rejects_attributes_that_begin_inside_the_update_sequence_array() {
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        r[20..22].copy_from_slice(&48u16.to_le_bytes());
        assert!(!apply_fixup(&mut r, SECTOR));
    }

    #[test]
    fn accepts_attributes_that_begin_exactly_where_the_update_sequence_array_ends() {
        let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
        r[20..22].copy_from_slice(&54u16.to_le_bytes());
        assert_eq!(fixup_layout(&r, SECTOR), Some((48, 3)));
        assert!(apply_fixup(&mut r, SECTOR));
        assert_eq!(u16::from_le_bytes([r[510], r[511]]), 0xAAAA);
    }

    #[test]
    fn rejects_attributes_that_begin_at_or_past_the_end_of_the_record() {
        for attributes_offset in [1024u16, 2048] {
            let mut r = record_with_usa(1024, 48, 0x0001, &[0xAAAA, 0xBBBB]);
            r[20..22].copy_from_slice(&attributes_offset.to_le_bytes());
            assert!(!apply_fixup(&mut r, SECTOR));
        }
    }
}
