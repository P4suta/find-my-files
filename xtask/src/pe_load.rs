//! Minimal, fail-closed PE parser for the service's dependent-load policy.
//!
//! `fmf-service.exe` is the only executable launched through UAC while it still
//! lives in the user-writable extracted bundle. Its linker embeds
//! `DependentLoadFlags` in the PE Load Configuration Directory. Publish and
//! package both parse that field themselves so a missing linker flag cannot
//! silently turn an adjacent DLL into elevated code.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`, embedded by
/// `/DEPENDENTLOADFLAG:0x800`.
pub const SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS: u16 = 0x0800;

const DOS_PE_POINTER_OFFSET: usize = 0x3c;
const COFF_HEADER_SIZE: usize = 20;
const SECTION_HEADER_SIZE: usize = 40;
const LOAD_CONFIG_DIRECTORY_INDEX: usize = 10;
const DATA_DIRECTORY_SIZE: usize = 8;

pub fn require_system32_only(path: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("read PE {}", path.display()))?;
    let flags = dependent_load_flags(&bytes)
        .with_context(|| format!("parse PE load policy from {}", path.display()))?;
    if flags != SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS {
        bail!(
            "{} has DependentLoadFlags 0x{flags:04x}; expected exactly 0x{:04x} \
             (LOAD_LIBRARY_SEARCH_SYSTEM32)",
            path.display(),
            SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS
        );
    }
    Ok(())
}

fn dependent_load_flags(bytes: &[u8]) -> Result<u16> {
    if read_bytes(bytes, 0, 2, "DOS signature")? != b"MZ" {
        bail!("missing DOS MZ signature");
    }
    let pe_offset = read_u32(bytes, DOS_PE_POINTER_OFFSET, "DOS e_lfanew")? as usize;
    if read_bytes(bytes, pe_offset, 4, "PE signature")? != b"PE\0\0" {
        bail!("missing PE signature");
    }

    let coff = checked_add(pe_offset, 4, "COFF header offset")?;
    let section_count = read_u16(
        bytes,
        checked_add(coff, 2, "section count offset")?,
        "section count",
    )? as usize;
    if section_count == 0 {
        bail!("PE has no sections");
    }
    let optional_size = read_u16(
        bytes,
        checked_add(coff, 16, "optional size offset")?,
        "optional header size",
    )? as usize;
    let optional = checked_add(coff, COFF_HEADER_SIZE, "optional header offset")?;
    read_bytes(bytes, optional, optional_size, "optional header")?;

    let magic = read_u16(bytes, optional, "optional header magic")?;
    let (directory_count_offset, directories_offset, dependent_flags_offset) = match magic {
        0x10b => (92usize, 96usize, 54usize), // PE32 / IMAGE_LOAD_CONFIG_DIRECTORY32
        0x20b => (108usize, 112usize, 78usize), // PE32+ / IMAGE_LOAD_CONFIG_DIRECTORY64
        other => bail!("unsupported optional header magic 0x{other:04x}"),
    };
    let load_config_end = checked_add(
        directories_offset,
        checked_mul(
            LOAD_CONFIG_DIRECTORY_INDEX + 1,
            DATA_DIRECTORY_SIZE,
            "load-config directory end",
        )?,
        "load-config directory end",
    )?;
    if optional_size < load_config_end {
        bail!(
            "optional header is too short for load-config directory: {optional_size} < {load_config_end}"
        );
    }

    let directory_count = read_u32(
        bytes,
        checked_add(optional, directory_count_offset, "directory count offset")?,
        "number of data directories",
    )? as usize;
    if directory_count <= LOAD_CONFIG_DIRECTORY_INDEX {
        bail!("PE has no load-config data-directory entry");
    }

    let load_entry = checked_add(
        optional,
        checked_add(
            directories_offset,
            checked_mul(
                LOAD_CONFIG_DIRECTORY_INDEX,
                DATA_DIRECTORY_SIZE,
                "load-config directory offset",
            )?,
            "load-config directory offset",
        )?,
        "load-config directory offset",
    )?;
    let load_rva = read_u32(bytes, load_entry, "load-config RVA")?;
    let load_directory_size = read_u32(
        bytes,
        checked_add(load_entry, 4, "load-config size offset")?,
        "load-config size",
    )? as usize;
    let required_size = checked_add(dependent_flags_offset, 2, "DependentLoadFlags end")?;
    if load_rva == 0 {
        bail!("PE load-config directory is absent");
    }
    if load_directory_size < required_size {
        bail!(
            "load-config directory is too short for DependentLoadFlags: \
             {load_directory_size} < {required_size}"
        );
    }

    let size_of_headers = read_u32(
        bytes,
        checked_add(optional, 60, "SizeOfHeaders offset")?,
        "SizeOfHeaders",
    )?;
    let sections = checked_add(optional, optional_size, "section table offset")?;
    let section_table_size = checked_mul(section_count, SECTION_HEADER_SIZE, "section table size")?;
    read_bytes(bytes, sections, section_table_size, "section table")?;

    let load_offset = rva_to_file_offset(
        bytes,
        load_rva,
        required_size,
        size_of_headers,
        sections,
        section_count,
    )?;
    let structure_size = read_u32(bytes, load_offset, "IMAGE_LOAD_CONFIG_DIRECTORY.Size")? as usize;
    if structure_size < required_size {
        bail!(
            "IMAGE_LOAD_CONFIG_DIRECTORY.Size does not cover DependentLoadFlags: \
             {structure_size} < {required_size}"
        );
    }
    read_u16(
        bytes,
        checked_add(
            load_offset,
            dependent_flags_offset,
            "DependentLoadFlags file offset",
        )?,
        "DependentLoadFlags",
    )
}

fn rva_to_file_offset(
    bytes: &[u8],
    rva: u32,
    len: usize,
    size_of_headers: u32,
    sections: usize,
    section_count: usize,
) -> Result<usize> {
    let end_rva = (rva as u64)
        .checked_add(len as u64)
        .context("RVA range overflow")?;
    if rva < size_of_headers {
        if end_rva > size_of_headers as u64 {
            bail!("RVA range straddles PE headers and section data");
        }
        let offset = rva as usize;
        read_bytes(bytes, offset, len, "header-mapped RVA")?;
        return Ok(offset);
    }

    let mut match_offset = None;
    for index in 0..section_count {
        let section = checked_add(
            sections,
            checked_mul(index, SECTION_HEADER_SIZE, "section header offset")?,
            "section header offset",
        )?;
        let virtual_size = read_u32(
            bytes,
            checked_add(section, 8, "VirtualSize offset")?,
            "VirtualSize",
        )?;
        let virtual_address = read_u32(
            bytes,
            checked_add(section, 12, "VirtualAddress offset")?,
            "VirtualAddress",
        )?;
        let raw_size = read_u32(
            bytes,
            checked_add(section, 16, "SizeOfRawData offset")?,
            "SizeOfRawData",
        )?;
        let raw_pointer = read_u32(
            bytes,
            checked_add(section, 20, "PointerToRawData offset")?,
            "PointerToRawData",
        )?;

        let mapped_size = virtual_size.max(raw_size) as u64;
        let section_end = (virtual_address as u64)
            .checked_add(mapped_size)
            .context("section RVA range overflow")?;
        if (rva as u64) < virtual_address as u64 || end_rva > section_end {
            continue;
        }
        let delta = rva - virtual_address;
        let raw_end = (delta as u64)
            .checked_add(len as u64)
            .context("section raw range overflow")?;
        if raw_end > raw_size as u64 {
            bail!("RVA points into zero-filled rather than file-backed section data");
        }
        let offset_u64 = (raw_pointer as u64)
            .checked_add(delta as u64)
            .context("section file offset overflow")?;
        let offset = usize::try_from(offset_u64).context("section file offset exceeds usize")?;
        read_bytes(bytes, offset, len, "section-mapped RVA")?;
        if match_offset.replace(offset).is_some() {
            bail!("RVA maps ambiguously to overlapping PE sections");
        }
    }
    match_offset.context("load-config RVA does not map to file-backed PE data")
}

fn read_bytes<'a>(bytes: &'a [u8], offset: usize, len: usize, field: &str) -> Result<&'a [u8]> {
    let end = checked_add(offset, len, field)?;
    bytes
        .get(offset..end)
        .with_context(|| format!("truncated {field} at file offset 0x{offset:x}"))
}

fn read_u16(bytes: &[u8], offset: usize, field: &str) -> Result<u16> {
    let raw: [u8; 2] = read_bytes(bytes, offset, 2, field)?
        .try_into()
        .expect("length checked");
    Ok(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize, field: &str) -> Result<u32> {
    let raw: [u8; 4] = read_bytes(bytes, offset, 4, field)?
        .try_into()
        .expect("length checked");
    Ok(u32::from_le_bytes(raw))
}

fn checked_add(left: usize, right: usize, field: &str) -> Result<usize> {
    left.checked_add(right)
        .with_context(|| format!("{field} offset overflow"))
}

fn checked_mul(left: usize, right: usize, field: &str) -> Result<usize> {
    left.checked_mul(right)
        .with_context(|| format!("{field} size overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn pe_fixture(pe32_plus: bool, flags: u16) -> Vec<u8> {
        const PE: usize = 0x80;
        const OPTIONAL: usize = PE + 4 + COFF_HEADER_SIZE;
        const SECTION_RAW: usize = 0x400;
        const LOAD_CONFIG_RAW: usize = SECTION_RAW + 0x100;

        let optional_size = if pe32_plus { 0xf0 } else { 0xe0 };
        let directories_offset = if pe32_plus { 112 } else { 96 };
        let flags_offset = if pe32_plus { 78 } else { 54 };
        let mut bytes = vec![0u8; 0x800];
        bytes[..2].copy_from_slice(b"MZ");
        write_u32(&mut bytes, DOS_PE_POINTER_OFFSET, PE as u32);
        bytes[PE..PE + 4].copy_from_slice(b"PE\0\0");
        write_u16(&mut bytes, PE + 4 + 2, 1);
        write_u16(&mut bytes, PE + 4 + 16, optional_size as u16);
        write_u16(&mut bytes, OPTIONAL, if pe32_plus { 0x20b } else { 0x10b });
        write_u32(&mut bytes, OPTIONAL + 60, SECTION_RAW as u32);
        write_u32(&mut bytes, OPTIONAL + if pe32_plus { 108 } else { 92 }, 16);
        let load_entry = OPTIONAL + directories_offset + LOAD_CONFIG_DIRECTORY_INDEX * 8;
        write_u32(&mut bytes, load_entry, 0x1100);
        write_u32(&mut bytes, load_entry + 4, 0x100);

        let section = OPTIONAL + optional_size;
        bytes[section..section + 8].copy_from_slice(b".rdata\0\0");
        write_u32(&mut bytes, section + 8, 0x400);
        write_u32(&mut bytes, section + 12, 0x1000);
        write_u32(&mut bytes, section + 16, 0x400);
        write_u32(&mut bytes, section + 20, SECTION_RAW as u32);

        write_u32(&mut bytes, LOAD_CONFIG_RAW, 0x100);
        write_u16(&mut bytes, LOAD_CONFIG_RAW + flags_offset, flags);
        bytes
    }

    #[test]
    fn parses_system32_policy_from_pe32_and_pe32_plus() {
        for pe32_plus in [false, true] {
            assert_eq!(
                dependent_load_flags(&pe_fixture(pe32_plus, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS))
                    .unwrap(),
                SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS
            );
        }
    }

    #[test]
    fn policy_gate_rejects_any_other_flag_value() {
        for flags in [0, 0x0801, 0xffff] {
            let bytes = pe_fixture(true, flags);
            assert_eq!(dependent_load_flags(&bytes).unwrap(), flags);
        }
    }

    #[test]
    fn malformed_or_missing_load_config_fails_closed() {
        let mut no_directory = pe_fixture(true, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS);
        write_u32(&mut no_directory, 0x80 + 4 + COFF_HEADER_SIZE + 108, 10);
        assert!(dependent_load_flags(&no_directory).is_err());

        let mut short_directory = pe_fixture(true, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS);
        let load_entry = 0x80 + 4 + COFF_HEADER_SIZE + 112 + LOAD_CONFIG_DIRECTORY_INDEX * 8;
        write_u32(&mut short_directory, load_entry + 4, 79);
        assert!(dependent_load_flags(&short_directory).is_err());

        let mut short_structure = pe_fixture(true, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS);
        write_u32(&mut short_structure, 0x500, 79);
        assert!(dependent_load_flags(&short_structure).is_err());

        let mut unmapped = pe_fixture(true, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS);
        write_u32(&mut unmapped, load_entry, 0x9000);
        assert!(dependent_load_flags(&unmapped).is_err());

        assert!(dependent_load_flags(&[]).is_err());
        assert!(dependent_load_flags(
            &pe_fixture(true, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS)[..0x501]
        )
        .is_err());
    }

    #[test]
    fn adjacent_dll_does_not_change_embedded_policy_gate() {
        let base =
            std::env::temp_dir().join(format!("xtask-pe-load-adjacent-{}", std::process::id()));
        let _ = crate::fsx::force_remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let service = base.join("fmf-service.exe");
        fs::write(
            &service,
            pe_fixture(true, SYSTEM32_ONLY_DEPENDENT_LOAD_FLAGS),
        )
        .unwrap();
        fs::write(base.join("VCRUNTIME140.dll"), b"planted").unwrap();

        require_system32_only(&service).unwrap();

        fs::write(&service, pe_fixture(true, 0)).unwrap();
        assert!(require_system32_only(&service).is_err());
        crate::fsx::force_remove_dir_all(&base).unwrap();
    }
}
