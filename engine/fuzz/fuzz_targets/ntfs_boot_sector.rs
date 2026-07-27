#![no_main]
//! Fuzz the NTFS boot-sector decoder — the first bytes read from a mounted
//! volume, and the ones that decide every later offset. A crafted VHDX or USB
//! stick chooses all of them, and `fmf-service` parses them as `LocalSystem`,
//! so a geometry that overflows or panics is a denial of service on a
//! privileged process (ADR-0047).
//!
//! Input shaping. Raw bytes essentially never satisfy the two magic gates
//! (`"NTFS    "` at offset 3, `0x55AA` at 510), so a purely raw harness would
//! spend its whole budget on the first `return None`. The second construction
//! stamps only those two constants over a 512-byte sector and leaves *every*
//! geometry field — bytes-per-sector, sectors-per-cluster, total sectors, $MFT
//! cluster, the signed clusters-per-record code — under fuzzer control, so the
//! sanity checks themselves are what gets mutated.
//!
//! That alone is not enough: the derived arithmetic (`cluster_size`,
//! `volume_size`, `mft_position`, the record-size shift, and the containment
//! test that ties them together) sits behind a conjunction of two
//! power-of-two-in-range tests, which a mutator clears only by chance —
//! measured at 0 in 200k random inputs. The third construction therefore draws
//! just those two fields from the small set a real volume can declare and
//! leaves the sizes they are multiplied by fuzzed, so every `checked_mul`,
//! `checked_shl` and bound in the second half of the decoder is reached on
//! every input.

use fmf_core::ondisk::ntfs::decode_boot_sector;
use libfuzzer_sys::fuzz_target;

const SECTOR_BYTES: usize = 512;

fuzz_target!(|data: &[u8]| {
    // Fixed-offset control prefix: a byte's meaning must not shift when the
    // mutator changes the input's length.
    let control = |index: usize| u32::from(data.get(index).copied().unwrap_or(0));

    // Raw bytes at a deliberately unaligned address: the decoder may never
    // rely on the buffer's alignment, and short buffers must fail closed.
    let skew = control(0) as usize & 7;
    let mut skewed = vec![0xA5; skew];
    skewed.extend_from_slice(data);
    let _ = decode_boot_sector(&skewed[skew..]);

    // Past the magic: the whole geometry is attacker-chosen.
    let mut sector = [0u8; SECTOR_BYTES];
    let body = &data[..data.len().min(SECTOR_BYTES)];
    sector[..body.len()].copy_from_slice(body);
    sector[3..11].copy_from_slice(b"NTFS    ");
    sector[510..512].copy_from_slice(&[0x55, 0xAA]);
    let _ = decode_boot_sector(&sector);

    // Same sector, one byte off the natural alignment of every field it holds.
    let mut unaligned = vec![0xA5];
    unaligned.extend_from_slice(&sector);
    let _ = decode_boot_sector(&unaligned[1..]);

    // Past the two power-of-two gates as well: sector and cluster geometry
    // from the values a real boot sector declares, everything they scale
    // (total sectors, $MFT cluster, clusters-per-record) still fuzzed.
    let sector_size = 1u16 << (9 + control(1) % 4); // 512..=4096
    sector[11..13].copy_from_slice(&sector_size.to_le_bytes());
    sector[13] = 1 << (control(2) % 8); // 1..=128 sectors per cluster

    // A random 64-bit sector count or $MFT cluster overflows its scaling
    // multiply about 998 times in 1000, so leaving both raw hides the check
    // that matters most here — that the $MFT plus one record lands inside the
    // volume, i.e. that a crafted boot sector cannot aim later reads off the
    // end of the device. Narrow both fields (the overflow guards stay fuzzed
    // through the two raw constructions above) so that decision is reached.
    sector[46..48].fill(0); // total sectors: 48 bits
    sector[53..56].fill(0); // $MFT first cluster: 40 bits
    let _ = decode_boot_sector(&sector);
});
