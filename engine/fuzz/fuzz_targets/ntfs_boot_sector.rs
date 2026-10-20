#![no_main]
//! Exercise raw, unaligned, and shaped boot sectors while checking every accepted geometry.

use fmf_core::ondisk::ntfs::decode_boot_sector;
use libfuzzer_sys::fuzz_target;

const SECTOR_BYTES: usize = 512;
const MAX_SECTOR_BYTES: u64 = 4096;
const OEM_RANGE: core::ops::Range<usize> = 3..11;
const SIGNATURE_RANGE: core::ops::Range<usize> = 510..512;
const SIGNATURE: [u8; 2] = [0x55, 0xAA];
const SECTOR_SIZE_RANGE: core::ops::Range<usize> = 11..13;
const CLUSTER_SECTORS_OFFSET: usize = 13;
const TOTAL_SECTORS_HIGH_RANGE: core::ops::Range<usize> = 46..48;
const MFT_CLUSTER_HIGH_RANGE: core::ops::Range<usize> = 53..56;
const ALIGNMENT_SKEW_MASK: usize = 7;
const ALIGNMENT_PADDING: u8 = 0xA5;
const MIN_SECTOR_SHIFT: u32 = 9;
const SECTOR_SHIFT_CHOICES: u32 = 4;
const CLUSTER_SHIFT_CHOICES: u32 = 8;
const CLUSTER_CONTROL: usize = 2;

fn check_geometry(bytes: &[u8]) {
    if let Some(geometry) = decode_boot_sector(bytes) {
        assert!(geometry.sector_size.is_power_of_two());
        assert!((SECTOR_BYTES as u64..=MAX_SECTOR_BYTES).contains(&geometry.sector_size));
        assert!(geometry.cluster_size.is_power_of_two());
        assert!(geometry.cluster_size.is_multiple_of(geometry.sector_size));
        assert!(geometry.file_record_size.is_power_of_two());
        assert!(geometry
            .mft_position
            .checked_add(geometry.file_record_size)
            .is_some_and(|end| end <= geometry.volume_size));
    }
}

fuzz_target!(|data: &[u8]| {
    let control = |index: usize| u32::from(data.get(index).copied().unwrap_or(0));
    let skew = control(0) as usize & ALIGNMENT_SKEW_MASK;
    let mut skewed = vec![ALIGNMENT_PADDING; skew];
    skewed.extend_from_slice(data);
    assert_eq!(
        decode_boot_sector(data),
        decode_boot_sector(&skewed[skew..])
    );
    check_geometry(data);

    let mut sector = [0u8; SECTOR_BYTES];
    let body = &data[..data.len().min(SECTOR_BYTES)];
    sector[..body.len()].copy_from_slice(body);
    sector[OEM_RANGE].copy_from_slice(b"NTFS    ");
    sector[SIGNATURE_RANGE].copy_from_slice(&SIGNATURE);
    check_geometry(&sector);

    let mut unaligned = vec![ALIGNMENT_PADDING];
    unaligned.extend_from_slice(&sector);
    assert_eq!(
        decode_boot_sector(&sector),
        decode_boot_sector(&unaligned[1..])
    );

    let sector_size = 1u16 << (MIN_SECTOR_SHIFT + control(1) % SECTOR_SHIFT_CHOICES);
    sector[SECTOR_SIZE_RANGE].copy_from_slice(&sector_size.to_le_bytes());
    sector[CLUSTER_SECTORS_OFFSET] = 1 << (control(CLUSTER_CONTROL) % CLUSTER_SHIFT_CHOICES);
    sector[TOTAL_SECTORS_HIGH_RANGE].fill(0);
    sector[MFT_CLUSTER_HIGH_RANGE].fill(0);
    check_geometry(&sector);
});
