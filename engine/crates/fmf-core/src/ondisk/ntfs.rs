//! Alignment-independent decoding of the NTFS on-disk structures used by the scanner.
//!
//! Disk bytes are untrusted and may begin at any address.
//! This module never casts them to Rust references: every multi-byte field is copied into an owned integer with an explicit little-endian conversion.
//! Keeping that invariant in one module prevents packed-struct references from leaking into the parallel initial scan or the live-USN reconciliation path.

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::num::{NonZeroI8, NonZeroU8, NonZeroUsize};
use core::ops::Range;

const MAX_SECTOR_BYTES: usize = 4096;
const UTF16_UNIT_BYTES: usize = size_of::<u16>();
const ATTRIBUTE_ALIGNMENT: usize = 8;
const SEQUENCE_SHIFT: u32 = 48;
const RESIDENT_LENGTH_OFFSET: usize = 16;
const RESIDENT_VALUE_OFFSET: usize = 20;
const LOWEST_VCN_OFFSET: usize = 16;
const HIGHEST_VCN_OFFSET: usize = 24;
const MAPPING_PAIRS_OFFSET: usize = 32;
const STREAM_SIZE_OFFSET: usize = 48;
const MODIFICATION_TIME_OFFSET: usize = 8;
const FILE_ATTRIBUTES_OFFSET: usize = 32;
const FILENAME_UNITS_OFFSET: usize = 64;
const FILENAME_NAMESPACE_OFFSET: usize = 65;
const BOOT_OEM_RANGE: core::ops::Range<usize> = 3..11;
const BOOT_SIGNATURE_RANGE: core::ops::Range<usize> = 510..512;
const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];
const BYTES_PER_SECTOR_OFFSET: usize = 11;
const SECTORS_PER_CLUSTER_OFFSET: usize = 13;
const TOTAL_SECTORS_OFFSET: usize = 40;
const MFT_CLUSTER_OFFSET: usize = 48;
const RECORD_SIZE_CODE_OFFSET: usize = 64;
const FILE_SEQUENCE_OFFSET: usize = 16;
const FILE_ATTRIBUTES_OFFSET_FIELD: usize = 20;
const FILE_FLAGS_OFFSET: usize = 22;
const FILE_USED_BYTES_OFFSET: usize = 24;
const FILE_ALLOCATED_BYTES_OFFSET: usize = 28;
const FILE_BASE_REFERENCE_OFFSET: usize = 32;
const ATTRIBUTE_LENGTH_OFFSET: usize = 4;
const ATTRIBUTE_RESIDENCY_OFFSET: usize = 8;
const ATTRIBUTE_NAME_UNITS_OFFSET: usize = 9;
const ATTRIBUTE_NAME_OFFSET_FIELD: usize = 10;
const ATTRIBUTE_FLAGS_OFFSET: usize = 12;
const ATTRIBUTE_ID_OFFSET: usize = 14;

const FILE_RECORD_HEADER_BYTES: usize = 42;
const ATTRIBUTE_HEADER_BYTES: usize = 16;
const RESIDENT_HEADER_BYTES: usize = 24;
/// Wire size of a non-resident attribute header, before its mapping pairs.
pub const NONRESIDENT_HEADER_BYTES: usize = 64;
/// Wire size of one `$ATTRIBUTE_LIST` entry header, before its stream name.
pub const ATTRIBUTE_LIST_ENTRY_BYTES: usize = 26;
const STANDARD_INFORMATION_BYTES: usize = 36;
const FILE_NAME_HEADER_BYTES: usize = 66;
const FILE_RECORD_SIGNATURE: &[u8; 4] = b"FILE";
const FILE_RECORD_IN_USE: u16 = 0x0001;
const FILE_RECORD_DIRECTORY: u16 = 0x0002;
const SECTOR_BYTES: usize = 512;
const MAX_FILE_RECORD_BYTES: u64 = 64 << 10;

/// Record number of the volume's root directory.
pub const ROOT_RECORD: u64 = 5;
/// Record number of `\$Extend`.
///
/// It is the one metadata file that is a directory with children of its own.
/// Those children (`$Quota`, `$ObjId`, `$Reparse`, `$UsnJrnl`, `$RmMetadata`) live at or above [`FIRST_NORMAL_RECORD`], so they are indexed — and a parent that is not indexed is a parent that cannot resolve.
pub const EXTEND_RECORD: u64 = 11;
/// First record number that is not an NTFS metadata file.
pub const FIRST_NORMAL_RECORD: u64 = 24;

/// The attribute types the scanner decodes; every other type is skipped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NtfsAttributeType {
    /// `$STANDARD_INFORMATION` — timestamps and file attribute flags.
    StandardInformation = 0x10,
    /// `$ATTRIBUTE_LIST` — pointers to attributes held in extension records.
    AttributeList = 0x20,
    /// `$FILE_NAME` — one hard link (parent reference plus UTF-16 name).
    FileName = 0x30,
    /// `$DATA` — the unnamed data stream, needed for the $MFT's own run map.
    Data = 0x80,
    /// End-of-chain sentinel that terminates a record's attribute list.
    End = 0xFFFF_FFFF,
}

/// Namespace of a `$FILE_NAME` link, which decides whether it is searchable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NtfsFileNamespace {
    /// Case-sensitive POSIX name.
    Posix = 0,
    /// Long Win32 name.
    Win32 = 1,
    /// Short 8.3 name paired with a separate Win32 name.
    Dos = 2,
    /// A name that is simultaneously the Win32 and the 8.3 name.
    Win32AndDos = 3,
}

/// Decoded fixed prefix of a `FILE` record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtfsFileRecordHeader {
    /// Reuse counter; combined with the record number to form an FRN.
    pub sequence_value: u16,
    /// Byte offset of the first attribute within the record.
    pub attributes_offset: u16,
    /// `IN_USE` (0x1) and `DIRECTORY` (0x2) bits.
    pub flags: u16,
    /// Bytes of the record actually occupied by the attribute chain.
    pub used_size: u32,
    /// Total record size; must equal the buffer length.
    pub allocated_size: u32,
    /// Base record reference, or 0 when this record is itself a base.
    pub base_reference: u64,
}

/// Decoded common header shared by resident and non-resident attributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtfsAttributeHeader {
    /// Attribute type code (see [`NtfsAttributeType`]).
    pub type_id: u32,
    /// Total attribute length in bytes, header included.
    pub length: u32,
    /// 0 for resident, 1 for non-resident; any other value is rejected.
    pub is_non_resident: u8,
    /// UTF-16 code-unit count of the optional stream name.
    pub name_length: u8,
    /// Byte offset of the stream name within the attribute.
    pub name_offset: u16,
    /// Compression/encryption/sparse flags.
    pub flags: u16,
    /// Instance id, unique within the record.
    pub id: u16,
}

/// Decoded extra header carried only by non-resident attributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtfsNonResidentAttributeHeader {
    /// First virtual cluster this extent maps.
    pub lowest_vcn: i64,
    /// Last virtual cluster this extent maps.
    pub highest_vcn: i64,
    /// Byte offset of the mapping pairs within the attribute.
    pub data_runs_offset: u16,
    /// Logical size of the whole stream, in bytes.
    pub data_size: u64,
}

/// The two `$STANDARD_INFORMATION` fields the index stores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtfsStandardInformation {
    /// Last-write time, in Windows FILETIME ticks.
    pub modification_time: u64,
    /// `FILE_ATTRIBUTE_*` bits.
    pub file_attributes: u32,
}

/// Decoded fixed prefix of a `$FILE_NAME` attribute value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NtfsFileNameHeader {
    /// FRN of the directory holding this link.
    pub parent_directory_reference: u64,
    /// UTF-16 code-unit count of the name.
    pub name_length: u8,
    /// Raw namespace byte (see [`NtfsFileNamespace`]).
    pub namespace: u8,
}

/// One hard link: its header plus the borrowed name bytes.
#[derive(Clone, Copy)]
pub struct NtfsFileName<'a> {
    /// Parent reference, length and namespace.
    pub header: NtfsFileNameHeader,
    /// Exact UTF-16LE bytes borrowed from the fixed-up record.
    pub utf16le: &'a [u8],
}

impl<'a> NtfsFileName<'a> {
    fn parse(value: &'a [u8]) -> Option<Self> {
        let name_units = NonZeroU8::new(*value.get(FILENAME_UNITS_OFFSET)?)?;
        let name_length = usize::from(name_units.get());
        let name_bytes = name_length.checked_mul(size_of::<u16>())?;
        let end = FILE_NAME_HEADER_BYTES.checked_add(name_bytes)?;
        if value.len() != end {
            return None;
        }
        let encoded = value.get(FILE_NAME_HEADER_BYTES..end)?;
        if encoded
            .as_chunks::<UTF16_UNIT_BYTES>()
            .0
            .iter()
            .any(|pair| {
                let unit = u16::from_le_bytes([pair[0], pair[1]]);
                unit == 0 || unit == u16::from(b'/') || unit == u16::from(b'\\')
            })
        {
            return None;
        }
        let parent_directory_reference = le_u64(value, 0)?;
        if parent_directory_reference >> SEQUENCE_SHIFT == 0 {
            return None;
        }
        let namespace = *value.get(FILENAME_NAMESPACE_OFFSET)?;
        if namespace > NtfsFileNamespace::Win32AndDos as u8 {
            return None;
        }
        Some(Self {
            header: NtfsFileNameHeader {
                parent_directory_reference,
                name_length: name_units.get(),
                namespace,
            },
            utf16le: encoded,
        })
    }

    /// Copy the borrowed name into owned UTF-16 code units.
    ///
    /// Unpaired surrogates are preserved: NTFS names are not required to be valid Unicode, and the index stores them as WTF-8.
    #[must_use]
    pub fn to_utf16(self) -> Vec<u16> {
        self.utf16le
            .as_chunks::<UTF16_UNIT_BYTES>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }
}

/// One bounds-checked attribute inside a fixed-up record.
pub struct NtfsAttribute<'a> {
    data: &'a [u8],
    /// The decoded common header.
    pub header: NtfsAttributeHeader,
    length: NonZeroUsize,
}

impl<'a> NtfsAttribute<'a> {
    /// Decode the attribute starting at `data[0]`, or `None` if its declared length, residency, or name range escapes the buffer.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let header = decode_attribute_header(data)?;
        let length = usize::try_from(header.length).ok()?;
        if header.type_id == NtfsAttributeType::End as u32
            || length > data.len()
            || !matches!(header.is_non_resident, 0 | 1)
        {
            return None;
        }
        let wire_header_bytes = if header.is_non_resident == 0 {
            RESIDENT_HEADER_BYTES
        } else {
            NONRESIDENT_HEADER_BYTES
        };
        if length < wire_header_bytes {
            return None;
        }
        if header.name_length > 0 {
            let name_offset = usize::from(header.name_offset);
            let name_bytes = usize::from(header.name_length).checked_mul(size_of::<u16>())?;
            if name_offset < wire_header_bytes
                || name_offset
                    .checked_add(name_bytes)
                    .is_none_or(|end| end > length)
            {
                return None;
            }
        }
        Some(Self {
            data,
            header,
            length: NonZeroUsize::new(length)?,
        })
    }

    /// Total attribute length in bytes, header included.
    #[must_use]
    #[expect(
        clippy::len_without_is_empty,
        reason = "byte length of a wire structure, not a collection: a decoded \
                  attribute is never empty (parse rejects any length below the \
                  header size), so an is_empty() would always be false"
    )]
    pub const fn len(&self) -> usize {
        self.length.get()
    }

    /// The attribute's own bytes, clamped to its declared length.
    #[must_use]
    pub fn data(&self) -> &'a [u8] {
        &self.data[..self.length.get()]
    }

    /// The resident value bytes, or `None` for a non-resident attribute or an offset/length pair that escapes the attribute.
    #[must_use]
    pub fn get_resident(&self) -> Option<&'a [u8]> {
        self.data().get(self.resident_value_range()?)
    }

    /// The bounded range of a resident value, including empty values.
    pub(super) fn resident_value_range(&self) -> Option<Range<usize>> {
        if self.header.is_non_resident != 0 {
            return None;
        }
        let value_length = usize::try_from(le_u32(self.data(), RESIDENT_LENGTH_OFFSET)?).ok()?;
        let value_offset = usize::from(le_u16(self.data(), RESIDENT_VALUE_OFFSET)?);
        if value_offset < RESIDENT_HEADER_BYTES {
            return None;
        }
        let end = value_offset.checked_add(value_length)?;
        self.data().get(value_offset..end)?;
        Some(value_offset..end)
    }

    /// The declared resident value length, without slicing it out.
    /// Lets a caller distinguish "declared too large" from "absent".
    #[must_use]
    pub fn resident_value_length(&self) -> Option<u32> {
        if self.header.is_non_resident != 0 {
            return None;
        }
        le_u32(self.data(), RESIDENT_LENGTH_OFFSET)
    }

    /// The non-resident header, or `None` unless this is a non-resident attribute whose mapping-pair offset stays inside it.
    #[must_use]
    pub fn nonresident_header(&self) -> Option<NtfsNonResidentAttributeHeader> {
        if self.header.is_non_resident != 1 {
            return None;
        }
        let data = self.data().get(..NONRESIDENT_HEADER_BYTES)?;
        Some(NtfsNonResidentAttributeHeader {
            lowest_vcn: le_i64(data, LOWEST_VCN_OFFSET)?,
            highest_vcn: le_i64(data, HIGHEST_VCN_OFFSET)?,
            data_runs_offset: le_u16(data, MAPPING_PAIRS_OFFSET)?,
            data_size: le_u64(data, STREAM_SIZE_OFFSET)?,
        })
        .filter(|header| {
            let offset = usize::from(header.data_runs_offset);
            offset >= NONRESIDENT_HEADER_BYTES && offset <= self.length.get()
        })
    }

    /// Decode this attribute as `$STANDARD_INFORMATION`, or `None` if it is a different type or too short.
    #[must_use]
    pub fn as_standard_info(&self) -> Option<NtfsStandardInformation> {
        if self.header.type_id != NtfsAttributeType::StandardInformation as u32 {
            return None;
        }
        let value = self.get_resident()?;
        if value.len() < STANDARD_INFORMATION_BYTES {
            return None;
        }
        Some(NtfsStandardInformation {
            modification_time: le_u64(value, MODIFICATION_TIME_OFFSET)?,
            file_attributes: le_u32(value, FILE_ATTRIBUTES_OFFSET)?,
        })
    }

    /// Decode this attribute as `$FILE_NAME`.
    ///
    /// Returns `None` for a different type, a value whose length does not match its declared name length exactly, a name containing NUL or a path separator, a link with no parent, or an unknown namespace — a crafted volume must not be able to inject a path into the index.
    #[must_use]
    pub fn as_name(&self) -> Option<NtfsFileName<'a>> {
        if self.header.type_id != NtfsAttributeType::FileName as u32 {
            return None;
        }
        NtfsFileName::parse(self.get_resident()?)
    }
}

/// One validated `FILE` record, borrowed from a fixed-up buffer.
pub struct NtfsFile<'a> {
    /// $MFT record number this buffer was read from.
    pub number: u64,
    /// The decoded record header.
    pub header: NtfsFileRecordHeader,
    /// The whole fixed-up record buffer.
    pub data: &'a [u8],
}

impl<'a> NtfsFile<'a> {
    /// Validate `data` as record `number` and borrow it, or `None` if the signature, update-sequence geometry, or size fields disagree with `sector_size` and the buffer length.
    #[must_use]
    pub fn parse(number: u64, data: &'a [u8], sector_size: usize) -> Option<Self> {
        let header = decode_file_header(data)?;
        valid_file_header(data, header, sector_size).then_some(Self {
            number,
            header,
            data,
        })
    }

    /// The same validation as [`NtfsFile::parse`] without borrowing, for use before the fixup is applied.
    #[must_use]
    pub fn is_valid(data: &[u8], sector_size: usize) -> bool {
        decode_file_header(data).is_some_and(|header| valid_file_header(data, header, sector_size))
    }

    /// The record's FRN: sequence value in the high 16 bits, record number in the low 48.
    #[must_use]
    pub const fn reference_number(&self) -> u64 {
        let [r0, r1, r2, r3, r4, r5, _, _] = self.number.to_le_bytes();
        let [s0, s1] = self.header.sequence_value.to_le_bytes();
        u64::from_le_bytes([r0, r1, r2, r3, r4, r5, s0, s1])
    }

    /// Visit every decodable attribute in chain order, stopping at the first terminator, malformed attribute, or overrun of the used region.
    pub fn attributes(&self, mut visit: impl FnMut(&NtfsAttribute<'a>)) {
        for attribute in self.attribute_chain() {
            visit(&attribute);
        }
    }

    /// The first attribute of `attribute_type` in chain order, if any.
    #[must_use]
    pub fn get_attribute(&self, attribute_type: NtfsAttributeType) -> Option<NtfsAttribute<'a>> {
        self.attribute_chain()
            .find(|attribute| attribute.header.type_id == attribute_type as u32)
    }

    fn attribute_chain(&self) -> impl Iterator<Item = NtfsAttribute<'a>> {
        let used = usize::try_from(self.header.used_size)
            .map_or(self.data.len(), |value| value.min(self.data.len()));
        let mut remaining = self
            .data
            .get(usize::from(self.header.attributes_offset)..used);
        core::iter::from_fn(move || {
            let data = remaining.take()?;
            if le_u32(data, 0)? == NtfsAttributeType::End as u32 {
                return None;
            }
            let attribute = NtfsAttribute::parse(data)?;
            remaining = data.get(attribute.length.get()..);
            Some(attribute)
        })
    }

    /// True when the record is allocated rather than a deleted remnant.
    #[must_use]
    pub const fn is_used(&self) -> bool {
        self.header.flags & FILE_RECORD_IN_USE != 0
    }

    /// True when the record describes a directory.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.header.flags & FILE_RECORD_DIRECTORY != 0
    }
}

/// Volume layout as declared by the NTFS boot sector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VolumeGeometry {
    /// Bytes per sector (512–4096, power of two).
    pub sector_size: u64,
    /// Bytes per cluster.
    pub cluster_size: u64,
    /// Total volume size in bytes.
    pub volume_size: u64,
    /// Bytes per $MFT file record.
    pub file_record_size: u64,
    /// Byte offset of the $MFT's first cluster.
    pub mft_position: u64,
}

/// Decode and sanity-check the NTFS boot sector.
///
/// Every derived quantity is range- and overflow-checked against the others,
/// so a crafted boot sector cannot produce a geometry that later drives an out-of-bounds read: the $MFT must start inside the volume and one record must fit after it.
#[must_use]
pub fn decode_boot_sector(data: &[u8]) -> Option<VolumeGeometry> {
    let data = data.get(..SECTOR_BYTES)?;
    if data.get(BOOT_OEM_RANGE)? != b"NTFS    " || data.get(BOOT_SIGNATURE_RANGE)? != BOOT_SIGNATURE
    {
        return None;
    }
    let sector_size = u64::from(le_u16(data, BYTES_PER_SECTOR_OFFSET)?);
    let sectors_per_cluster = NonZeroU8::new(*data.get(SECTORS_PER_CLUSTER_OFFSET)?)?;
    if !(SECTOR_BYTES as u64..=MAX_SECTOR_BYTES as u64).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || !sectors_per_cluster.get().is_power_of_two()
    {
        return None;
    }
    let cluster_size = sector_size.checked_mul(u64::from(sectors_per_cluster.get()))?;
    let volume_size = le_u64(data, TOTAL_SECTORS_OFFSET)?.checked_mul(sector_size)?;
    let mft_position = le_u64(data, MFT_CLUSTER_OFFSET)?.checked_mul(cluster_size)?;
    let record_code = NonZeroI8::new(i8::from_le_bytes([*data.get(RECORD_SIZE_CODE_OFFSET)?]))?;
    let file_record_size = if record_code.get().is_positive() {
        u64::try_from(record_code.get())
            .ok()?
            .checked_mul(cluster_size)?
    } else {
        let shift = u32::from(record_code.get().checked_abs()?.cast_unsigned());
        1u64.checked_shl(shift)?
    };
    if file_record_size < sector_size
        || file_record_size > MAX_FILE_RECORD_BYTES
        || !file_record_size.is_power_of_two()
        || mft_position
            .checked_add(file_record_size)
            .is_none_or(|end| end > volume_size)
    {
        return None;
    }
    Some(VolumeGeometry {
        sector_size,
        cluster_size,
        volume_size,
        file_record_size,
        mft_position,
    })
}

fn decode_file_header(data: &[u8]) -> Option<NtfsFileRecordHeader> {
    let data = data.get(..FILE_RECORD_HEADER_BYTES)?;
    if data.get(..FILE_RECORD_SIGNATURE.len())? != FILE_RECORD_SIGNATURE {
        return None;
    }
    Some(NtfsFileRecordHeader {
        sequence_value: le_u16(data, FILE_SEQUENCE_OFFSET)?,
        attributes_offset: le_u16(data, FILE_ATTRIBUTES_OFFSET_FIELD)?,
        flags: le_u16(data, FILE_FLAGS_OFFSET)?,
        used_size: le_u32(data, FILE_USED_BYTES_OFFSET)?,
        allocated_size: le_u32(data, FILE_ALLOCATED_BYTES_OFFSET)?,
        base_reference: le_u64(data, FILE_BASE_REFERENCE_OFFSET)?,
    })
}

fn valid_file_header(data: &[u8], header: NtfsFileRecordHeader, sector_size: usize) -> bool {
    if super::fixup::fixup_layout(data, sector_size).is_none() {
        return false;
    }
    let Ok(used) = usize::try_from(header.used_size) else {
        return false;
    };
    let attributes_offset = usize::from(header.attributes_offset);
    let in_use = header.flags & FILE_RECORD_IN_USE != 0;
    usize::try_from(header.allocated_size).ok() == Some(data.len())
        && (!in_use || header.sequence_value != 0)
        && (header.base_reference == 0 || header.base_reference >> SEQUENCE_SHIFT != 0)
        && used <= data.len()
        && attributes_offset.is_multiple_of(ATTRIBUTE_ALIGNMENT)
        && attributes_offset < used
}

fn decode_attribute_header(data: &[u8]) -> Option<NtfsAttributeHeader> {
    let data = data.get(..ATTRIBUTE_HEADER_BYTES)?;
    Some(NtfsAttributeHeader {
        type_id: le_u32(data, 0)?,
        length: le_u32(data, ATTRIBUTE_LENGTH_OFFSET)?,
        is_non_resident: *data.get(ATTRIBUTE_RESIDENCY_OFFSET)?,
        name_length: *data.get(ATTRIBUTE_NAME_UNITS_OFFSET)?,
        name_offset: le_u16(data, ATTRIBUTE_NAME_OFFSET_FIELD)?,
        flags: le_u16(data, ATTRIBUTE_FLAGS_OFFSET)?,
        id: le_u16(data, ATTRIBUTE_ID_OFFSET)?,
    })
}

fn le_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = data
        .get(offset..offset.checked_add(size_of::<u16>())?)?
        .try_into()
        .ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn le_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = data
        .get(offset..offset.checked_add(size_of::<u32>())?)?
        .try_into()
        .ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn le_u64(data: &[u8], offset: usize) -> Option<u64> {
    let bytes: [u8; 8] = data
        .get(offset..offset.checked_add(size_of::<u64>())?)?
        .try_into()
        .ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn le_i64(data: &[u8], offset: usize) -> Option<i64> {
    let bytes: [u8; 8] = data
        .get(offset..offset.checked_add(size_of::<i64>())?)?
        .try_into()
        .ok()?;
    Some(i64::from_le_bytes(bytes))
}

#[cfg(kani)]
#[path = "ntfs_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const USA_OFFSET_FIELD: usize = 4;
    const USA_LENGTH_FIELD: usize = 6;

    fn put_u16(data: &mut [u8], offset: usize, value: u16) {
        data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(data: &mut [u8], offset: usize, value: u64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn boot_sector_decode_is_checked_and_alignment_independent() {
        let mut bytes = vec![0xA5; SECTOR_BYTES + 1];
        let boot = &mut bytes[1..];
        boot[3..11].copy_from_slice(b"NTFS    ");
        boot[510..512].copy_from_slice(&[0x55, 0xAA]);
        put_u16(boot, 11, 512);
        boot[13] = 8;
        put_u64(boot, 40, 2_000_000);
        put_u64(boot, 48, 4);
        boot[64] = (-10i8).cast_unsigned();

        assert_eq!(
            decode_boot_sector(boot),
            Some(VolumeGeometry {
                sector_size: 512,
                cluster_size: 4096,
                volume_size: 1_024_000_000,
                file_record_size: 1024,
                mft_position: 16_384,
            })
        );
        boot[13] = 3;
        assert!(decode_boot_sector(boot).is_none());
    }

    #[test]
    fn attribute_decode_never_requires_aligned_storage() {
        let mut storage = [0x5A; NONRESIDENT_HEADER_BYTES + 2];
        let skew = 1 + (storage.as_ptr().addr() & 1);
        let attribute = &mut storage[skew..skew + NONRESIDENT_HEADER_BYTES];
        assert_eq!(
            attribute.as_ptr().addr() & 1,
            1,
            "storage must be unaligned"
        );
        attribute.fill(0);
        put_u32(attribute, 0, NtfsAttributeType::Data as u32);
        put_u32(attribute, 4, NONRESIDENT_HEADER_BYTES as u32);
        attribute[8] = 1;
        put_u64(attribute, 16, 7);
        put_u64(attribute, 24, 9);
        put_u16(attribute, 32, NONRESIDENT_HEADER_BYTES as u16);
        put_u64(attribute, 48, 0x0123_4567_89AB_CDEF);

        let parsed = NtfsAttribute::parse(attribute).expect("valid non-resident attribute");
        assert_eq!(
            parsed.nonresident_header(),
            Some(NtfsNonResidentAttributeHeader {
                lowest_vcn: 7,
                highest_vcn: 9,
                data_runs_offset: NONRESIDENT_HEADER_BYTES as u16,
                data_size: 0x0123_4567_89AB_CDEF,
            })
        );
    }

    #[test]
    fn file_record_allocation_must_match_exact_record_buffer() {
        let mut record = vec![0u8; 1024];
        record[..4].copy_from_slice(FILE_RECORD_SIGNATURE);
        put_u16(&mut record, 4, FILE_RECORD_HEADER_BYTES as u16);
        put_u16(&mut record, 6, 3);
        put_u16(&mut record, 20, 48);
        put_u32(&mut record, 24, 52);
        put_u32(&mut record, 28, 1024);
        assert!(NtfsFile::is_valid(&record, 512));

        put_u32(&mut record, 28, 2048);
        assert!(!NtfsFile::is_valid(&record, 512));
    }

    #[test]
    fn file_name_borrows_only_the_exact_utf16le_payload() {
        let mut attribute = vec![0u8; 120];
        put_u32(&mut attribute, 0, NtfsAttributeType::FileName as u32);
        put_u32(&mut attribute, 4, 96);
        put_u32(&mut attribute, 16, 70);
        put_u16(&mut attribute, 20, RESIDENT_HEADER_BYTES as u16);
        put_u64(&mut attribute, RESIDENT_HEADER_BYTES, (1u64 << 48) | 0x1234);
        attribute[RESIDENT_HEADER_BYTES + 64] = 2;
        attribute[RESIDENT_HEADER_BYTES + 65] = NtfsFileNamespace::Win32 as u8;
        attribute[RESIDENT_HEADER_BYTES + 66..RESIDENT_HEADER_BYTES + 70]
            .copy_from_slice(&[b'A', 0, 0x00, 0xD8]);

        let name = NtfsAttribute::parse(&attribute)
            .and_then(|attribute| attribute.as_name())
            .expect("valid resident FILE_NAME");
        assert_eq!(name.utf16le, [b'A', 0, 0x00, 0xD8]);
        assert_eq!(name.to_utf16(), [b'A' as u16, 0xD800]);
    }

    #[test]
    fn truncated_structures_fail_closed() {
        for length in 0..ATTRIBUTE_HEADER_BYTES {
            assert!(NtfsAttribute::parse(&vec![0; length]).is_none());
        }
        for length in 0..SECTOR_BYTES {
            assert!(decode_boot_sector(&vec![0; length]).is_none());
        }
    }

    fn resident_bytes(type_id: NtfsAttributeType, value: &[u8]) -> Vec<u8> {
        let length = (RESIDENT_HEADER_BYTES + value.len()).next_multiple_of(ATTRIBUTE_ALIGNMENT);
        let mut bytes = vec![0; length];
        put_u32(&mut bytes, 0, type_id as u32);
        put_u32(&mut bytes, ATTRIBUTE_LENGTH_OFFSET, length as u32);
        put_u32(&mut bytes, RESIDENT_LENGTH_OFFSET, value.len() as u32);
        put_u16(
            &mut bytes,
            RESIDENT_VALUE_OFFSET,
            RESIDENT_HEADER_BYTES as u16,
        );
        bytes[RESIDENT_HEADER_BYTES..RESIDENT_HEADER_BYTES + value.len()].copy_from_slice(value);
        bytes
    }

    fn name_bytes(units: &[u16], namespace: u8) -> Vec<u8> {
        let mut value = vec![0; FILE_NAME_HEADER_BYTES + units.len() * UTF16_UNIT_BYTES];
        put_u64(&mut value, 0, (1u64 << SEQUENCE_SHIFT) | ROOT_RECORD);
        value[FILENAME_UNITS_OFFSET] = units.len() as u8;
        value[FILENAME_NAMESPACE_OFFSET] = namespace;
        for (pair, unit) in value[FILE_NAME_HEADER_BYTES..]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(units)
        {
            pair.copy_from_slice(&unit.to_le_bytes());
        }
        resident_bytes(NtfsAttributeType::FileName, &value)
    }

    #[test]
    fn fixed_headers_decode_the_complete_prefix_at_the_exact_wire_size() {
        let mut file = [0; FILE_RECORD_HEADER_BYTES];
        file[..4].copy_from_slice(FILE_RECORD_SIGNATURE);
        put_u16(&mut file, FILE_SEQUENCE_OFFSET, 7);
        put_u32(&mut file, FILE_ALLOCATED_BYTES_OFFSET, 1024);
        for length in 0..=file.len() {
            let header = decode_file_header(&file[..length]);
            assert_eq!(header.is_some(), length == FILE_RECORD_HEADER_BYTES);
            if let Some(header) = header {
                assert_eq!(header.sequence_value, 7);
                assert_eq!(header.allocated_size, 1024);
            }
        }

        let mut attribute = [0; ATTRIBUTE_HEADER_BYTES];
        put_u32(&mut attribute, 0, NtfsAttributeType::Data as u32);
        put_u16(&mut attribute, ATTRIBUTE_ID_OFFSET, 129);
        for length in 0..=attribute.len() {
            let header = decode_attribute_header(&attribute[..length]);
            assert_eq!(header.is_some(), length == ATTRIBUTE_HEADER_BYTES);
            if let Some(header) = header {
                assert_eq!(header.id, 129);
            }
        }
    }

    #[test]
    fn references_keep_record_bytes_separate_from_sequence_bytes() {
        for number in [0, 1, 0x1234_5678_9ABC_DEF0, u64::MAX] {
            for sequence_value in [0, 1, u16::MAX] {
                let file = NtfsFile {
                    number,
                    data: &[],
                    header: NtfsFileRecordHeader {
                        sequence_value,
                        attributes_offset: 0,
                        flags: 0,
                        used_size: 0,
                        allocated_size: 0,
                        base_reference: 0,
                    },
                };
                let reference = file.reference_number().to_le_bytes();
                assert_eq!(&reference[..6], &number.to_le_bytes()[..6]);
                assert_eq!(&reference[6..], &sequence_value.to_le_bytes());
            }
        }
    }

    #[test]
    fn resident_values_include_empty_values_and_the_required_standard_info_prefix() {
        let empty = resident_bytes(NtfsAttributeType::Data, &[]);
        let attribute = NtfsAttribute::parse(&empty).expect("empty resident attribute");
        assert_eq!(attribute.get_resident(), Some(&[][..]));
        assert_eq!(attribute.resident_value_length(), Some(0));

        let mut value = vec![0; STANDARD_INFORMATION_BYTES];
        put_u64(&mut value, MODIFICATION_TIME_OFFSET, 0x0123_4567_89AB_CDEF);
        put_u32(&mut value, FILE_ATTRIBUTES_OFFSET, 0x1234);
        let bytes = resident_bytes(NtfsAttributeType::StandardInformation, &value);
        assert_eq!(
            NtfsAttribute::parse(&bytes).and_then(|attribute| attribute.as_standard_info()),
            Some(NtfsStandardInformation {
                modification_time: 0x0123_4567_89AB_CDEF,
                file_attributes: 0x1234,
            })
        );
        let short = resident_bytes(
            NtfsAttributeType::StandardInformation,
            &value[..value.len() - 1],
        );
        assert!(
            NtfsAttribute::parse(&short)
                .and_then(|attribute| attribute.as_standard_info())
                .is_none()
        );

        let mut nonresident = vec![0; NONRESIDENT_HEADER_BYTES];
        put_u32(&mut nonresident, 0, NtfsAttributeType::Data as u32);
        put_u32(
            &mut nonresident,
            ATTRIBUTE_LENGTH_OFFSET,
            NONRESIDENT_HEADER_BYTES as u32,
        );
        nonresident[ATTRIBUTE_RESIDENCY_OFFSET] = 1;
        let attribute = NtfsAttribute::parse(&nonresident).expect("non-resident header");
        assert_eq!(attribute.resident_value_length(), None);
        assert_eq!(attribute.get_resident(), None);
    }

    #[test]
    fn declared_attribute_lengths_cover_both_header_boundaries() {
        for residency in [0, 1] {
            for length in 0..=74 {
                let mut bytes = vec![0; 72];
                put_u32(&mut bytes, 0, NtfsAttributeType::Data as u32);
                put_u32(&mut bytes, ATTRIBUTE_LENGTH_OFFSET, length);
                bytes[ATTRIBUTE_RESIDENCY_OFFSET] = residency;
                let minimum = if residency == 0 { 24 } else { 64 };
                let parsed = NtfsAttribute::parse(&bytes);
                assert_eq!(
                    parsed.is_some(),
                    (minimum..=72).contains(&length),
                    "residency {residency}, length {length}"
                );
                if let Some(attribute) = parsed {
                    assert_eq!(attribute.len(), length as usize);
                    assert_eq!(attribute.data().len(), length as usize);
                }
            }
        }
        let mut value = vec![0; 48];
        put_u16(&mut value, MAPPING_PAIRS_OFFSET - RESIDENT_HEADER_BYTES, 64);
        let bytes = resident_bytes(NtfsAttributeType::Data, &value);
        let attribute =
            NtfsAttribute::parse(&bytes).expect("resident data with header-shaped bytes");
        assert_eq!(attribute.nonresident_header(), None);
    }

    #[test]
    fn attribute_names_fit_the_declared_attribute_and_ignore_unused_offsets() {
        for residency in [0, 1] {
            let minimum = if residency == 0 { 24 } else { 64 };
            for length in minimum..=minimum + 8 {
                for name_units in [0u8, 1, 2] {
                    for name_offset in [0, minimum - 1, minimum, minimum + 1, u16::MAX] {
                        let mut bytes = vec![0; 96];
                        put_u32(&mut bytes, 0, NtfsAttributeType::Data as u32);
                        put_u32(&mut bytes, ATTRIBUTE_LENGTH_OFFSET, u32::from(length));
                        bytes[ATTRIBUTE_RESIDENCY_OFFSET] = residency;
                        bytes[ATTRIBUTE_NAME_UNITS_OFFSET] = name_units;
                        put_u16(&mut bytes, ATTRIBUTE_NAME_OFFSET_FIELD, name_offset);
                        let expected = name_units == 0
                            || (name_offset >= minimum
                                && u32::from(name_offset) + u32::from(name_units) * 2
                                    <= u32::from(length));
                        assert_eq!(
                            NtfsAttribute::parse(&bytes).is_some(),
                            expected,
                            "residency {residency}, length {length}, name {name_offset}+{name_units}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn nonresident_mapping_offsets_must_fit_both_header_and_attribute() {
        for offset in [0, 63, 64, 65, 72, u16::MAX] {
            let mut bytes = vec![0; 72];
            put_u32(&mut bytes, 0, NtfsAttributeType::Data as u32);
            put_u32(&mut bytes, ATTRIBUTE_LENGTH_OFFSET, 72);
            bytes[ATTRIBUTE_RESIDENCY_OFFSET] = 1;
            put_u16(&mut bytes, MAPPING_PAIRS_OFFSET, offset);
            let attribute = NtfsAttribute::parse(&bytes).expect("bounded non-resident attribute");
            assert_eq!(
                attribute.nonresident_header().is_some(),
                (64..=72).contains(&offset),
                "mapping offset {offset}"
            );
        }
    }

    #[test]
    fn names_accept_every_namespace_and_the_full_length_field() {
        for namespace in 0..=u8::MAX {
            let bytes = name_bytes(&[u16::from(b'X'); 255], namespace);
            let name = NtfsAttribute::parse(&bytes).and_then(|attribute| attribute.as_name());
            assert_eq!(name.is_some(), namespace <= 3, "namespace {namespace}");
            if let Some(name) = name {
                assert_eq!(name.header.name_length, 255);
                assert_eq!(name.to_utf16(), [u16::from(b'X'); 255]);
            }
        }
        for forbidden in [0, u16::from(b'/'), u16::from(b'\\')] {
            for units in [vec![forbidden], vec![u16::from(b'X'), forbidden]] {
                let bytes = name_bytes(&units, 0);
                assert!(
                    NtfsAttribute::parse(&bytes)
                        .and_then(|attribute| attribute.as_name())
                        .is_none()
                );
            }
        }
        let empty = name_bytes(&[], 0);
        assert!(
            NtfsAttribute::parse(&empty)
                .and_then(|attribute| attribute.as_name())
                .is_none()
        );
        for (reference, accepted) in [
            (0, false),
            (1, false),
            (1 << 48, true),
            ((1 << 48) | 1, true),
            (u64::MAX, true),
        ] {
            let mut bytes = name_bytes(&[u16::from(b'X')], 0);
            put_u64(&mut bytes, RESIDENT_HEADER_BYTES, reference);
            assert_eq!(
                NtfsAttribute::parse(&bytes)
                    .and_then(|attribute| attribute.as_name())
                    .is_some(),
                accepted,
                "parent reference {reference}"
            );
        }
    }

    fn boot_bytes(sector_size: u16, sectors_per_cluster: u8, record_code: i8) -> Vec<u8> {
        let mut bytes = vec![0; SECTOR_BYTES];
        bytes[BOOT_OEM_RANGE].copy_from_slice(b"NTFS    ");
        bytes[BOOT_SIGNATURE_RANGE].copy_from_slice(&BOOT_SIGNATURE);
        put_u16(&mut bytes, BYTES_PER_SECTOR_OFFSET, sector_size);
        bytes[SECTORS_PER_CLUSTER_OFFSET] = sectors_per_cluster;
        put_u64(&mut bytes, TOTAL_SECTORS_OFFSET, 2_000_000);
        put_u64(&mut bytes, MFT_CLUSTER_OFFSET, 4);
        bytes[RECORD_SIZE_CODE_OFFSET] = record_code.cast_unsigned();
        bytes
    }

    #[test]
    fn boot_geometry_checks_independent_signatures_and_record_size_endpoints() {
        let valid = boot_bytes(512, 8, -10);
        let mut padded = valid.clone();
        padded.resize(1024, 0xA5);
        assert_eq!(decode_boot_sector(&padded), decode_boot_sector(&valid));
        for offset in [BOOT_OEM_RANGE.start, BOOT_SIGNATURE_RANGE.start] {
            let mut bytes = valid.clone();
            bytes[offset] ^= 1;
            assert!(decode_boot_sector(&bytes).is_none());
        }
        for sector_size in [256, 511, 513, 8192] {
            assert!(decode_boot_sector(&boot_bytes(sector_size, 8, -10)).is_none());
        }
        for sectors_per_cluster in 0..=u8::MAX {
            assert_eq!(
                decode_boot_sector(&boot_bytes(512, sectors_per_cluster, -10)).is_some(),
                sectors_per_cluster.is_power_of_two(),
                "sectors per cluster {sectors_per_cluster}"
            );
        }
        for (record_code, accepted) in [(-8, false), (-9, true), (-16, true), (-17, false)] {
            let geometry = decode_boot_sector(&boot_bytes(512, 8, record_code));
            assert_eq!(geometry.is_some(), accepted, "record code {record_code}");
            if let Some(geometry) = geometry {
                assert_eq!(
                    geometry.file_record_size,
                    1u64 << record_code.unsigned_abs()
                );
            }
        }
        let largest_fields = decode_boot_sector(&boot_bytes(4096, 128, -16))
            .expect("largest admitted sector and cluster fields");
        assert_eq!(largest_fields.cluster_size, 4096 * 128);
        for (record_code, accepted) in [(0, false), (1, true), (2, true), (3, false)] {
            let geometry = decode_boot_sector(&boot_bytes(512, 1, record_code));
            assert_eq!(
                geometry.is_some(),
                accepted,
                "cluster record code {record_code}"
            );
            if let Some(geometry) = geometry {
                assert_eq!(geometry.file_record_size, record_code as u64 * 512);
            }
        }
        let mut exact_end = boot_bytes(512, 1, -9);
        put_u64(&mut exact_end, TOTAL_SECTORS_OFFSET, 5);
        let geometry = decode_boot_sector(&exact_end).expect("record ends at the volume boundary");
        assert_eq!(
            geometry.mft_position + geometry.file_record_size,
            geometry.volume_size
        );
        put_u64(&mut exact_end, TOTAL_SECTORS_OFFSET, 4);
        assert!(decode_boot_sector(&exact_end).is_none());
    }

    #[test]
    fn file_base_references_require_the_sequence_field_for_extension_records() {
        let mut bytes = vec![0; 512];
        bytes[..4].copy_from_slice(FILE_RECORD_SIGNATURE);
        put_u16(&mut bytes, USA_OFFSET_FIELD, 48);
        put_u16(&mut bytes, USA_LENGTH_FIELD, 2);
        put_u16(&mut bytes, FILE_ATTRIBUTES_OFFSET_FIELD, 56);
        put_u16(&mut bytes, FILE_SEQUENCE_OFFSET, 1);
        put_u16(&mut bytes, FILE_FLAGS_OFFSET, FILE_RECORD_IN_USE);
        put_u32(&mut bytes, FILE_USED_BYTES_OFFSET, 64);
        put_u32(&mut bytes, FILE_ALLOCATED_BYTES_OFFSET, 512);
        for (reference, accepted) in [
            (0, true),
            (1, false),
            (1 << 48, true),
            ((1 << 48) | 1, true),
            (u64::MAX, true),
        ] {
            put_u64(&mut bytes, FILE_BASE_REFERENCE_OFFSET, reference);
            assert_eq!(
                NtfsFile::is_valid(&bytes, 512),
                accepted,
                "base reference {reference}"
            );
        }
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_and_offsets_never_panic_or_escape(
            prefix in 0usize..8,
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
        ) {
            let mut storage = vec![0xA5; prefix];
            storage.extend_from_slice(&bytes);
            let input = &storage[prefix..];

            let _ = decode_boot_sector(input);
            let _ = NtfsFile::is_valid(input, 512);
            if let Some(file) = NtfsFile::parse(7, input, 512) {
                file.attributes(|attribute| {
                    let _ = attribute.get_resident();
                    let _ = attribute.resident_value_length();
                    let _ = attribute.nonresident_header();
                    let _ = attribute.as_standard_info();
                    let _ = attribute.as_name();
                });
            }
            if let Some(attribute) = NtfsAttribute::parse(input) {
                let _ = attribute.get_resident();
                let _ = attribute.resident_value_length();
                let _ = attribute.nonresident_header();
                let _ = attribute.as_standard_info();
                let _ = attribute.as_name();
            }
        }
    }
}
