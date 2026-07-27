//! The NTFS update-sequence array (fixup) applied to one record buffer.
//!
//! Every $MFT / $LogFile-style record has its last two bytes per sector
//! replaced by a sentinel before it is written, and the displaced bytes are
//! parked in an update-sequence array in the record header. Undoing that is
//! pure byte arithmetic over untrusted disk bytes, so it lives here next to
//! the rest of the on-disk grammar rather than beside the Windows volume
//! handles that fetch the buffer.

#![forbid(unsafe_code)]

const SECTOR: usize = 512;

/// Apply the NTFS update sequence array in place. Returns false when the
/// sector check bytes don't match (torn/corrupt record).
#[must_use]
pub fn apply_fixup(data: &mut [u8], sector_size: usize) -> bool {
    let Some((uso, usl)) = fixup_layout(data, sector_size) else {
        return false;
    };
    let usn = [data[uso], data[uso + 1]];
    let fixups: Vec<[u8; 2]> = (1..usl)
        .map(|i| {
            let usa_off = uso + i * 2;
            [data[usa_off], data[usa_off + 1]]
        })
        .collect();

    // Validate every sector before mutating any of them. Besides keeping a
    // failed record untouched, copying the USA first prevents a malicious USA
    // range that overlaps a sector tail from changing a later replacement.
    for sector in 1..usl {
        let sector_off = sector * sector_size - 2;
        if data[sector_off..sector_off + 2] != usn {
            return false;
        }
    }
    for (sector, fixup) in fixups.into_iter().enumerate() {
        let sector_off = (sector + 1) * sector_size - 2;
        data[sector_off..sector_off + 2].copy_from_slice(&fixup);
    }
    true
}

pub(crate) fn fixup_layout(data: &[u8], sector_size: usize) -> Option<(usize, usize)> {
    if data.len() < sector_size
        || !(SECTOR..=4096).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || !data.len().is_multiple_of(sector_size)
    {
        return None;
    }
    let uso = u16::from_le_bytes([data[4], data[5]]) as usize;
    let usl = u16::from_le_bytes([data[6], data[7]]) as usize;
    let expected_usl = data.len().checked_div(sector_size)?.checked_add(1)?;
    let usa_bytes = usl.checked_mul(2)?;
    let usa_end = uso.checked_add(usa_bytes)?;
    let attributes_offset = u16::from_le_bytes([data[20], data[21]]) as usize;
    if uso < 42
        || !uso.is_multiple_of(2)
        || usl != expected_usl
        || usa_end > data.len()
        || usa_end > attributes_offset
        || attributes_offset >= data.len()
    {
        return None;
    }
    Some((uso, usl))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `len`-byte record with an update-sequence array at `uso`
    /// carrying `usn` plus `fixups` (the bytes that belong at each sector
    /// tail), and write the `usn` sentinel into each sector tail so a correct
    /// `apply_fixup` succeeds and restores the `fixups`.
    fn record_with_usa(len: usize, uso: usize, usn: u16, fixups: &[u16]) -> Vec<u8> {
        let mut r = vec![0u8; len];
        let sector_size = len / fixups.len();
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
        r[4..6].copy_from_slice(&48u16.to_le_bytes()); // uso
        r[6..8].copy_from_slice(&1u16.to_le_bytes()); // usl = 1 (no fixups)
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
        r[4..6].copy_from_slice(&1020u16.to_le_bytes()); // uso near the end
        r[6..8].copy_from_slice(&8u16.to_le_bytes()); // uso + usl*2 > len
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
        // Two sectors ⇒ two fixups; the tails currently hold the sentinel and
        // must come back as 0xAAAA and 0xBBBB after the fixup.
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
        // Corrupt the second sector tail so it no longer matches the USN.
        r[1022] = 0x99;
        assert!(!apply_fixup(&mut r, SECTOR));
    }
}
