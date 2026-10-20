//! Alignment-independent decoding of the NTFS on-disk structures used by the scanner.
//!
//! Disk bytes are untrusted and may begin at any address.
//! This module never casts them to Rust references: every multi-byte field is copied into an owned integer with an explicit little-endian conversion.
//! Keeping that invariant in one module prevents packed-struct references from leaking into the parallel initial scan or the live-USN reconciliation path.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

const MAX_SECTOR_BYTES: usize = 4096;
const UTF16_UNIT_BYTES: usize = size_of::<u16>();
const MAX_FILENAME_UNITS: usize = 255;
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
const USA_OFFSET_FIELD: usize = 4;
const USA_LENGTH_FIELD: usize = 6;
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
const FILE_RECORD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const FILE_RECORD_IN_USE: u16 = 0x0001;
const FILE_RECORD_DIRECTORY: u16 = 0x0002;
const SECTOR_BYTES: usize = 512;
const MAX_CLUSTER_BYTES: u64 = 2 << 20;
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

impl NtfsFileName<'_> {
    /// Copy the borrowed name into owned UTF-16 code units.
    ///
    /// Unpaired surrogates are preserved: NTFS names are not required to be valid Unicode, and the index stores them as WTF-8.
    #[must_use]
    pub fn to_utf16(self) -> Vec<u16> {
        self.utf16le
            .chunks_exact(UTF16_UNIT_BYTES)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }
}

/// One bounds-checked attribute inside a fixed-up record.
pub struct NtfsAttribute<'a> {
    data: &'a [u8],
    /// The decoded common header.
    pub header: NtfsAttributeHeader,
    length: usize,
}

impl<'a> NtfsAttribute<'a> {
    /// Decode the attribute starting at `data[0]`, or `None` if its declared length, residency, or name range escapes the buffer.
    #[must_use]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let header = decode_attribute_header(data)?;
        let length = usize::try_from(header.length).ok()?;
        if header.type_id == NtfsAttributeType::End as u32
            || length < ATTRIBUTE_HEADER_BYTES
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
            length,
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
        self.length
    }

    /// The attribute's own bytes, clamped to its declared length.
    #[must_use]
    pub fn data(&self) -> &'a [u8] {
        &self.data[..self.length]
    }

    /// The resident value bytes, or `None` for a non-resident attribute or an offset/length pair that escapes the attribute.
    #[must_use]
    pub fn get_resident(&self) -> Option<&'a [u8]> {
        if self.header.is_non_resident != 0 || self.length < RESIDENT_HEADER_BYTES {
            return None;
        }
        let value_length = usize::try_from(le_u32(self.data(), RESIDENT_LENGTH_OFFSET)?).ok()?;
        let value_offset = usize::from(le_u16(self.data(), RESIDENT_VALUE_OFFSET)?);
        if value_offset < RESIDENT_HEADER_BYTES {
            return None;
        }
        let end = value_offset.checked_add(value_length)?;
        self.data().get(value_offset..end)
    }

    /// The declared resident value length, without slicing it out.
    /// Lets a caller distinguish "declared too large" from "absent".
    #[must_use]
    pub fn resident_value_length(&self) -> Option<u32> {
        if self.header.is_non_resident != 0 || self.length < RESIDENT_HEADER_BYTES {
            return None;
        }
        le_u32(self.data(), RESIDENT_LENGTH_OFFSET)
    }

    /// The non-resident header, or `None` unless this is a non-resident attribute whose mapping-pair offset stays inside it.
    #[must_use]
    pub fn nonresident_header(&self) -> Option<NtfsNonResidentAttributeHeader> {
        if self.header.is_non_resident != 1 || self.length < NONRESIDENT_HEADER_BYTES {
            return None;
        }
        Some(NtfsNonResidentAttributeHeader {
            lowest_vcn: le_i64(self.data(), LOWEST_VCN_OFFSET)?,
            highest_vcn: le_i64(self.data(), HIGHEST_VCN_OFFSET)?,
            data_runs_offset: le_u16(self.data(), MAPPING_PAIRS_OFFSET)?,
            data_size: le_u64(self.data(), STREAM_SIZE_OFFSET)?,
        })
        .filter(|header| {
            let offset = usize::from(header.data_runs_offset);
            offset >= NONRESIDENT_HEADER_BYTES && offset <= self.length
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
        let value = self.get_resident()?;
        if value.len() < FILE_NAME_HEADER_BYTES {
            return None;
        }
        let name_length = usize::from(*value.get(FILENAME_UNITS_OFFSET)?);
        if name_length == 0 || name_length > MAX_FILENAME_UNITS {
            return None;
        }
        let name_bytes = name_length.checked_mul(size_of::<u16>())?;
        let end = FILE_NAME_HEADER_BYTES.checked_add(name_bytes)?;
        if value.len() != end {
            return None;
        }
        let encoded = value.get(FILE_NAME_HEADER_BYTES..end)?;
        if encoded.chunks_exact(UTF16_UNIT_BYTES).any(|pair| {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            unit == 0 || unit == u16::from(b'/') || unit == u16::from(b'\\')
        }) {
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
        Some(NtfsFileName {
            header: NtfsFileNameHeader {
                parent_directory_reference,
                name_length: u8::try_from(name_length).ok()?,
                namespace,
            },
            utf16le: encoded,
        })
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
        (self.header.sequence_value as u64) << SEQUENCE_SHIFT | (self.number & FILE_RECORD_MASK)
    }

    /// Visit every decodable attribute in chain order, stopping at the first terminator, malformed attribute, or overrun of the used region.
    pub fn attributes(&self, mut visit: impl FnMut(&NtfsAttribute<'a>)) {
        let mut offset = usize::from(self.header.attributes_offset);
        let used = usize::try_from(self.header.used_size)
            .map_or(self.data.len(), |value| value.min(self.data.len()));
        let bounded = &self.data[..used];
        while let Some(type_id) = le_u32(bounded, offset) {
            if type_id == NtfsAttributeType::End as u32 {
                return;
            }
            let Some(attribute) = NtfsAttribute::parse(&bounded[offset..]) else {
                return;
            };
            let length = attribute.len();
            visit(&attribute);
            let Some(next) = offset.checked_add(length) else {
                return;
            };
            if next <= offset || next > used {
                return;
            }
            offset = next;
        }
    }

    /// The first attribute of `attribute_type` in chain order, if any.
    #[must_use]
    pub fn get_attribute(&self, attribute_type: NtfsAttributeType) -> Option<NtfsAttribute<'a>> {
        let mut offset = usize::from(self.header.attributes_offset);
        let used = usize::try_from(self.header.used_size)
            .ok()?
            .min(self.data.len());
        let bounded = &self.data[..used];
        while let Some(type_id) = le_u32(bounded, offset) {
            if type_id == NtfsAttributeType::End as u32 {
                return None;
            }
            let attribute = NtfsAttribute::parse(&bounded[offset..])?;
            if type_id == attribute_type as u32 {
                return Some(attribute);
            }
            let next = offset.checked_add(attribute.len())?;
            if next <= offset || next > used {
                return None;
            }
            offset = next;
        }
        None
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
    if data.len() < SECTOR_BYTES
        || data.get(BOOT_OEM_RANGE)? != b"NTFS    "
        || data.get(BOOT_SIGNATURE_RANGE)? != BOOT_SIGNATURE
    {
        return None;
    }
    let sector_size = u64::from(le_u16(data, BYTES_PER_SECTOR_OFFSET)?);
    let sectors_per_cluster = u64::from(*data.get(SECTORS_PER_CLUSTER_OFFSET)?);
    if !(SECTOR_BYTES as u64..=MAX_SECTOR_BYTES as u64).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || sectors_per_cluster == 0
        || !sectors_per_cluster.is_power_of_two()
    {
        return None;
    }
    let cluster_size = sector_size.checked_mul(sectors_per_cluster)?;
    if cluster_size > MAX_CLUSTER_BYTES {
        return None;
    }
    let volume_size = le_u64(data, TOTAL_SECTORS_OFFSET)?.checked_mul(sector_size)?;
    let mft_position = le_u64(data, MFT_CLUSTER_OFFSET)?.checked_mul(cluster_size)?;
    let record_code = i8::from_le_bytes([*data.get(RECORD_SIZE_CODE_OFFSET)?]);
    let file_record_size = if record_code > 0 {
        u64::try_from(record_code).ok()?.checked_mul(cluster_size)?
    } else {
        let shift = u32::from(record_code.checked_abs()?.cast_unsigned());
        1u64.checked_shl(shift)?
    };
    if volume_size == 0
        || file_record_size < sector_size
        || file_record_size > MAX_FILE_RECORD_BYTES
        || !file_record_size.is_power_of_two()
        || !file_record_size.is_multiple_of(sector_size)
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
    if data.len() < FILE_RECORD_HEADER_BYTES
        || data.get(..FILE_RECORD_SIGNATURE.len())? != FILE_RECORD_SIGNATURE
    {
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
    let Some(update_sequence_offset) = le_u16(data, USA_OFFSET_FIELD).map(usize::from) else {
        return false;
    };
    let Some(update_sequence_length) = le_u16(data, USA_LENGTH_FIELD).map(usize::from) else {
        return false;
    };
    let Some(update_sequence_bytes) = update_sequence_length.checked_mul(size_of::<u16>()) else {
        return false;
    };
    let Some(update_sequence_end) = update_sequence_offset.checked_add(update_sequence_bytes)
    else {
        return false;
    };
    let Ok(used) = usize::try_from(header.used_size) else {
        return false;
    };
    let attributes_offset = usize::from(header.attributes_offset);
    let in_use = header.flags & FILE_RECORD_IN_USE != 0;
    update_sequence_length > 1
        && update_sequence_offset >= FILE_RECORD_HEADER_BYTES
        && update_sequence_offset.is_multiple_of(UTF16_UNIT_BYTES)
        && update_sequence_end <= data.len()
        && update_sequence_end <= attributes_offset
        && (SECTOR_BYTES..=MAX_SECTOR_BYTES).contains(&sector_size)
        && sector_size.is_power_of_two()
        && data.len().is_multiple_of(sector_size)
        && update_sequence_length == data.len() / sector_size + 1
        && usize::try_from(header.allocated_size).ok() == Some(data.len())
        && (!in_use || header.sequence_value != 0)
        && (header.base_reference == 0 || header.base_reference >> SEQUENCE_SHIFT != 0)
        && used <= data.len()
        && attributes_offset >= FILE_RECORD_HEADER_BYTES
        && attributes_offset.is_multiple_of(ATTRIBUTE_ALIGNMENT)
        && attributes_offset < used
}

fn decode_attribute_header(data: &[u8]) -> Option<NtfsAttributeHeader> {
    if data.len() < ATTRIBUTE_HEADER_BYTES {
        return None;
    }
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
