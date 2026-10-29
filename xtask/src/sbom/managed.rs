//! ECMA-335 managed content identity with validated RVA relocation.

use super::{sbom_read_u16, sbom_read_u32, ManagedImage};
use anyhow::{bail, ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

#[path = "managed_format.rs"]
mod format;
use format::{
    ASSEMBLY, ASSEMBLY_FIXED_BYTES, ASSEMBLY_OS, ASSEMBLY_PROCESSOR, ASSEMBLY_REF,
    ASSEMBLY_REF_FIXED_BYTES, ASSEMBLY_REF_OS, ASSEMBLY_REF_PROCESSOR, CLASS_LAYOUT,
    CLASS_LAYOUT_FIXED_BYTES, CLR_ENTRY_POINT_OFFSET, CLR_FLAGS_OFFSET, CLR_NATIVE_ENTRY_POINT,
    CLR_RESOURCES_OFFSET, CLR_RESOURCES_SIZE_OFFSET, CMOD_OPTIONAL, CMOD_REQUIRED,
    COMPRESSED_FOUR_MASK, COMPRESSED_FOUR_MAX, COMPRESSED_FOUR_MIN, COMPRESSED_FOUR_TAIL_BYTES,
    COMPRESSED_FOUR_VALUE_MIN, COMPRESSED_ONE_MAX, COMPRESSED_TWO_MASK, COMPRESSED_TWO_MAX,
    COMPRESSED_TWO_MIN, CONSTANT, CUSTOM_ATTRIBUTE, CUSTOM_ATTRIBUTE_TAG_BITS,
    CUSTOM_ATTRIBUTE_TYPE_TAG_BITS, DECL_SECURITY, EH_FAT, EH_FAT_CLAUSE_BYTES, EH_HEADER_BYTES,
    EH_KIND_MASK, EH_MORE_SECTIONS, EH_SMALL_CLAUSE_BYTES, EH_TABLE, ELEMENT_BOOLEAN, ELEMENT_CHAR,
    ELEMENT_I1, ELEMENT_I2, ELEMENT_I4, ELEMENT_I8, ELEMENT_R4, ELEMENT_R8, ELEMENT_U1, ELEMENT_U2,
    ELEMENT_U4, ELEMENT_U8, ELEMENT_VALUE_TYPE, ENC_LOG, ENC_MAP, EVENT, EVENT_MAP, EVENT_PTR,
    EXPORTED_TYPE, FIELD, FIELD_LAYOUT, FIELD_MARSHAL, FIELD_PTR, FIELD_RVA, FIELD_SIGNATURE, FILE,
    GENERIC_PARAM, GENERIC_PARAM_CONSTRAINT, HEAP_BLOBS, HEAP_FLAGS_OFFSET, HEAP_FLAGS_SUPPORTED,
    HEAP_GUIDS, HEAP_STRINGS, IMPL_MAP, INTERFACE_IMPL, MANIFEST_RESOURCE, MAX_TABLE_ROWS,
    MEMBER_REF, MEMBER_REF_TAG_BITS, METHOD_ALIGNMENT, METHOD_CODE_TYPE_BASE, METHOD_DEF,
    METHOD_DEF_FIXED_BYTES, METHOD_FAT, METHOD_FAT_HEADER_BYTES, METHOD_FAT_HEADER_WORDS,
    METHOD_FORMAT_MASK, METHOD_HEADER_SIZE_SHIFT, METHOD_IMPL, METHOD_IMPL_FLAGS_OFFSET,
    METHOD_MORE_SECTIONS, METHOD_PTR, METHOD_SEMANTICS, METHOD_SPEC, METHOD_TINY,
    METHOD_TINY_SIZE_SHIFT, METHOD_UNSUPPORTED_FLAGS, MODULE, MODULE_GUID_COLUMNS, MODULE_REF,
    NESTED_CLASS, OS_FIXED_BYTES, PARAM, PARAM_PTR, PROPERTY, PROPERTY_MAP, PROPERTY_PTR,
    SMALL_INDEX_LIMIT, STANDALONE_SIG, STATIC_RVA_FIELD_FLAGS, TABLE_COUNT, TABLE_HEADER_BYTES,
    TYPE_DEF, TYPE_REF, TYPE_REF_TAG_BASE, TYPE_REF_TAG_BITS, TYPE_REF_TAG_MASK, TYPE_SPEC,
    VALID_TABLES_OFFSET,
};

pub(super) struct Normalized {
    pub tables: Vec<u8>,
    pub content: Vec<u8>,
}

struct Tables<'a> {
    bytes: &'a [u8],
    rows: [u32; TABLE_COUNT],
    offsets: [usize; TABLE_COUNT],
    widths: [usize; TABLE_COUNT],
    strings: usize,
    blobs: usize,
}

impl<'a> Tables<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        ensure!(
            bytes.len() >= TABLE_HEADER_BYTES,
            "truncated CLR table header"
        );
        let heaps = bytes[HEAP_FLAGS_OFFSET];
        ensure!(
            heaps & !HEAP_FLAGS_SUPPORTED == 0,
            "unsupported CLR heap flags"
        );
        let valid = u64::from_le_bytes(
            bytes[VALID_TABLES_OFFSET..VALID_TABLES_OFFSET + size_of::<u64>()].try_into()?,
        );
        ensure!(
            valid >> TABLE_COUNT == 0,
            "unsupported CLR table identifier"
        );
        let unoptimized = [
            FIELD_PTR,
            METHOD_PTR,
            PARAM_PTR,
            EVENT_PTR,
            PROPERTY_PTR,
            ENC_MAP,
        ]
        .into_iter()
        .fold(0_u64, |mask, table| mask | (1 << table));
        ensure!(
            valid & unoptimized == 0,
            "unoptimized CLR pointer tables are unsupported"
        );
        let mut rows = [0; TABLE_COUNT];
        let mut cursor = TABLE_HEADER_BYTES;
        for (index, count) in rows.iter_mut().enumerate() {
            if valid & (1_u64 << index) != 0 {
                *count = sbom_read_u32(bytes, cursor, "table row count")?;
                ensure!(
                    *count <= MAX_TABLE_ROWS,
                    "CLR row count exceeds the token range"
                );
                cursor += size_of::<u32>();
            }
        }
        let mut tables = Self {
            bytes,
            rows,
            offsets: [0; TABLE_COUNT],
            widths: [0; TABLE_COUNT],
            strings: heap_width(heaps, HEAP_STRINGS),
            blobs: heap_width(heaps, HEAP_BLOBS),
        };
        for (index, count) in rows.iter().enumerate() {
            let width = tables.row_width(index, heap_width(heaps, HEAP_GUIDS))?;
            let range = checked_table_range(cursor, *count as usize, width, bytes.len())
                .with_context(|| format!("truncated or overflowing CLR table {index}"))?;
            tables.offsets[index] = range.start;
            tables.widths[index] = width;
            cursor = range.end;
        }
        Ok(tables)
    }

    const fn index_width(&self, table: usize) -> usize {
        if self.rows[table] < SMALL_INDEX_LIMIT {
            size_of::<u16>()
        } else {
            size_of::<u32>()
        }
    }

    fn coded_width(&self, bits: u32, tables: &[usize]) -> usize {
        if tables
            .iter()
            .all(|table| self.rows[*table] < (1 << (u16::BITS - bits)))
        {
            size_of::<u16>()
        } else {
            size_of::<u32>()
        }
    }

    fn row_width(&self, table: usize, guid: usize) -> Result<usize> {
        let s = self.strings;
        let b = self.blobs;
        let i = |table| self.index_width(table);
        let c = |bits, tables: &[usize]| self.coded_width(bits, tables);
        let type_ref = c(TYPE_REF_TAG_BITS, &[TYPE_DEF, TYPE_REF, TYPE_SPEC]);
        let method_ref = c(1, &[METHOD_DEF, MEMBER_REF]);
        let implementation = c(2, &[FILE, ASSEMBLY_REF, EXPORTED_TYPE]);
        Ok(match table {
            MODULE => size_of::<u16>() + s + MODULE_GUID_COLUMNS * guid,
            TYPE_REF => c(2, &[MODULE, MODULE_REF, ASSEMBLY_REF, TYPE_REF]) + 2 * s,
            TYPE_DEF => size_of::<u32>() + 2 * s + type_ref + i(FIELD) + i(METHOD_DEF),
            FIELD_PTR | FIELD_LAYOUT | FIELD_RVA => {
                i(FIELD)
                    + if table == FIELD_PTR {
                        0
                    } else {
                        size_of::<u32>()
                    }
            }
            FIELD | PROPERTY => size_of::<u16>() + s + b,
            METHOD_PTR => i(METHOD_DEF),
            METHOD_DEF => METHOD_DEF_FIXED_BYTES + s + b + i(PARAM),
            PARAM_PTR => i(PARAM),
            PARAM => size_of::<u32>() + s,
            INTERFACE_IMPL => i(TYPE_DEF) + type_ref,
            MEMBER_REF => {
                c(
                    MEMBER_REF_TAG_BITS,
                    &[TYPE_DEF, TYPE_REF, MODULE_REF, METHOD_DEF, TYPE_SPEC],
                ) + s
                    + b
            }
            CONSTANT => size_of::<u16>() + c(2, &[FIELD, PARAM, PROPERTY]) + b,
            CUSTOM_ATTRIBUTE => {
                c(
                    CUSTOM_ATTRIBUTE_TAG_BITS,
                    &[
                        METHOD_DEF,
                        FIELD,
                        TYPE_REF,
                        TYPE_DEF,
                        PARAM,
                        INTERFACE_IMPL,
                        MEMBER_REF,
                        MODULE,
                        DECL_SECURITY,
                        PROPERTY,
                        EVENT,
                        STANDALONE_SIG,
                        MODULE_REF,
                        TYPE_SPEC,
                        ASSEMBLY,
                        ASSEMBLY_REF,
                        FILE,
                        EXPORTED_TYPE,
                        MANIFEST_RESOURCE,
                        GENERIC_PARAM,
                        GENERIC_PARAM_CONSTRAINT,
                        METHOD_SPEC,
                    ],
                ) + c(CUSTOM_ATTRIBUTE_TYPE_TAG_BITS, &[METHOD_DEF, MEMBER_REF])
                    + b
            }
            FIELD_MARSHAL => c(1, &[FIELD, PARAM]) + b,
            DECL_SECURITY => size_of::<u16>() + c(2, &[TYPE_DEF, METHOD_DEF, ASSEMBLY]) + b,
            CLASS_LAYOUT => CLASS_LAYOUT_FIXED_BYTES + i(TYPE_DEF),
            STANDALONE_SIG | TYPE_SPEC => b,
            EVENT_MAP => i(TYPE_DEF) + i(EVENT),
            EVENT_PTR => i(EVENT),
            EVENT => size_of::<u16>() + s + type_ref,
            PROPERTY_MAP => i(TYPE_DEF) + i(PROPERTY),
            PROPERTY_PTR => i(PROPERTY),
            METHOD_SEMANTICS => size_of::<u16>() + i(METHOD_DEF) + c(1, &[EVENT, PROPERTY]),
            METHOD_IMPL => i(TYPE_DEF) + 2 * method_ref,
            MODULE_REF => s,
            IMPL_MAP => size_of::<u16>() + c(1, &[FIELD, METHOD_DEF]) + s + i(MODULE_REF),
            ENC_LOG => 2 * size_of::<u32>(),
            ENC_MAP | ASSEMBLY_PROCESSOR => size_of::<u32>(),
            ASSEMBLY => ASSEMBLY_FIXED_BYTES + b + 2 * s,
            ASSEMBLY_OS => OS_FIXED_BYTES,
            ASSEMBLY_REF => ASSEMBLY_REF_FIXED_BYTES + 2 * b + 2 * s,
            ASSEMBLY_REF_PROCESSOR => size_of::<u32>() + i(ASSEMBLY_REF),
            ASSEMBLY_REF_OS => OS_FIXED_BYTES + i(ASSEMBLY_REF),
            FILE => size_of::<u32>() + s + b,
            EXPORTED_TYPE => 2 * size_of::<u32>() + 2 * s + implementation,
            MANIFEST_RESOURCE => 2 * size_of::<u32>() + s + implementation,
            NESTED_CLASS => 2 * i(TYPE_DEF),
            GENERIC_PARAM => 2 * size_of::<u16>() + c(1, &[TYPE_DEF, METHOD_DEF]) + s,
            METHOD_SPEC => method_ref + b,
            GENERIC_PARAM_CONSTRAINT => i(GENERIC_PARAM) + type_ref,
            _ => bail!("unsupported CLR table {table}"),
        })
    }

    fn row(&self, table: usize, row: usize) -> Result<&[u8]> {
        ensure!(
            row < self.rows[table] as usize,
            "CLR row index is outside table {table}"
        );
        let start = self.offsets[table] + row * self.widths[table];
        Ok(&self.bytes[start..start + self.widths[table]])
    }

    fn index(bytes: &[u8], offset: usize, width: usize) -> Result<usize> {
        Ok(if width == 2 {
            usize::from(sbom_read_u16(bytes, offset, "CLR index")?)
        } else {
            sbom_read_u32(bytes, offset, "CLR index")? as usize
        })
    }

    fn class_sizes(&self) -> Result<BTreeMap<usize, usize>> {
        let mut sizes = BTreeMap::new();
        for index in 0..self.rows[CLASS_LAYOUT] as usize {
            let row = self.row(CLASS_LAYOUT, index)?;
            let parent = Self::index(row, CLASS_LAYOUT_FIXED_BYTES, self.index_width(TYPE_DEF))?;
            ensure!(
                parent != 0 && parent <= self.rows[TYPE_DEF] as usize,
                "ClassLayout has an invalid parent"
            );
            let size = sbom_read_u32(row, 2, "ClassLayout size")? as usize;
            ensure!(
                sizes.insert(parent, size).is_none(),
                "duplicate ClassLayout parent"
            );
        }
        Ok(sizes)
    }

    fn field_size(
        &self,
        field: usize,
        blobs: &[u8],
        classes: &BTreeMap<usize, usize>,
    ) -> Result<usize> {
        let row = self.row(
            FIELD,
            field.checked_sub(1).context("zero FieldRVA field index")?,
        )?;
        ensure!(
            sbom_read_u16(row, 0, "field flags")? & STATIC_RVA_FIELD_FLAGS
                == STATIC_RVA_FIELD_FLAGS,
            "FieldRVA must reference a static RVA field"
        );
        let index = Self::index(row, 2 + self.strings, self.blobs)?;
        ensure!(index != 0, "FieldRVA has an empty field signature");
        let mut signature = blobs
            .get(index..)
            .context("FieldRVA signature is outside the blob heap")?;
        let length = compressed(&mut signature)? as usize;
        signature = signature
            .get(..length)
            .context("truncated FieldRVA signature")?;
        ensure!(
            take_byte(&mut signature)? == FIELD_SIGNATURE,
            "invalid FieldRVA field signature"
        );
        while matches!(
            signature.first().copied(),
            Some(CMOD_REQUIRED | CMOD_OPTIONAL)
        ) {
            take_byte(&mut signature)?;
            let modifier = compressed(&mut signature)?;
            ensure!(
                modifier >> TYPE_REF_TAG_BITS != 0 && modifier & TYPE_REF_TAG_MASK <= 2,
                "invalid field custom modifier"
            );
        }
        let size = match take_byte(&mut signature)? {
            ELEMENT_BOOLEAN | ELEMENT_I1 | ELEMENT_U1 => size_of::<u8>(),
            ELEMENT_CHAR | ELEMENT_I2 | ELEMENT_U2 => size_of::<u16>(),
            ELEMENT_I4 | ELEMENT_U4 | ELEMENT_R4 => size_of::<u32>(),
            ELEMENT_I8 | ELEMENT_U8 | ELEMENT_R8 => size_of::<u64>(),
            ELEMENT_VALUE_TYPE => {
                let token = compressed(&mut signature)?;
                ensure!(
                    token.is_multiple_of(TYPE_REF_TAG_BASE),
                    "FieldRVA value type must have a local ClassLayout"
                );
                *classes
                    .get(&((token >> TYPE_REF_TAG_BITS) as usize))
                    .context("FieldRVA value type has no ClassLayout size")?
            }
            other => bail!("unsupported FieldRVA element type 0x{other:02x}"),
        };
        ensure!(
            signature.is_empty() && size != 0,
            "unsupported FieldRVA size or signature tail"
        );
        Ok(size)
    }
}

const fn heap_width(flags: u8, bit: u8) -> usize {
    if flags & bit == 0 {
        size_of::<u16>()
    } else {
        size_of::<u32>()
    }
}

fn checked_table_range(
    start: usize,
    rows: usize,
    width: usize,
    total: usize,
) -> Option<Range<usize>> {
    if width == 0 {
        return None;
    }
    let end = start.checked_add(rows.checked_mul(width)?)?;
    (end <= total).then_some(start..end)
}

fn take_byte(bytes: &mut &[u8]) -> Result<u8> {
    let (first, rest) = bytes.split_first().context("truncated CLR signature")?;
    *bytes = rest;
    Ok(*first)
}

fn compressed(bytes: &mut &[u8]) -> Result<u32> {
    let (value, width) =
        compressed_prefix(bytes).context("invalid or truncated CLR compressed integer")?;
    *bytes = &bytes[width..];
    Ok(value)
}

fn compressed_prefix(bytes: &[u8]) -> Option<(u32, usize)> {
    let first = *bytes.first()?;
    let (mut value, count, minimum) = match first {
        0..=COMPRESSED_ONE_MAX => return Some((u32::from(first), 1)),
        COMPRESSED_TWO_MIN..=COMPRESSED_TWO_MAX => (
            u32::from(first & COMPRESSED_TWO_MASK),
            1,
            u32::from(COMPRESSED_TWO_MIN),
        ),
        COMPRESSED_FOUR_MIN..=COMPRESSED_FOUR_MAX => (
            u32::from(first & COMPRESSED_FOUR_MASK),
            COMPRESSED_FOUR_TAIL_BYTES,
            COMPRESSED_FOUR_VALUE_MIN,
        ),
        _ => return None,
    };
    for byte in bytes.get(1..=count)? {
        value = (value << u8::BITS) | u32::from(*byte);
    }
    (value >= minimum).then_some((value, count + 1))
}

fn frame(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    output.extend_from_slice(bytes);
}

fn method_content(image: &ManagedImage<'_>, rva: usize, output: &mut Vec<u8>) -> Result<()> {
    let first = image.read_rva(rva, 1, "IL header")?[0];
    let (header_size, code_size, more_sections) = match first & METHOD_FORMAT_MASK {
        METHOD_TINY => (
            size_of::<u8>(),
            usize::from(first >> METHOD_TINY_SIZE_SHIFT),
            false,
        ),
        METHOD_FAT => {
            ensure!(
                rva.is_multiple_of(METHOD_ALIGNMENT),
                "unaligned fat IL header"
            );
            let header = image.read_rva(rva, METHOD_FAT_HEADER_BYTES, "fat IL header")?;
            let flags = sbom_read_u16(header, 0, "IL header flags")?;
            ensure!(
                flags >> METHOD_HEADER_SIZE_SHIFT == METHOD_FAT_HEADER_WORDS
                    && flags & METHOD_UNSUPPORTED_FLAGS == 0,
                "unsupported fat IL header"
            );
            (
                METHOD_FAT_HEADER_BYTES,
                sbom_read_u32(header, size_of::<u32>(), "IL code size")? as usize,
                flags & METHOD_MORE_SECTIONS != 0,
            )
        }
        _ => bail!("invalid IL header format"),
    };
    let length = header_size
        .checked_add(code_size)
        .context("IL body size overflow")?;
    frame(output, image.read_rva(rva, length, "IL body")?);
    let mut cursor = rva.checked_add(length).context("IL body RVA overflow")?;
    let mut more = more_sections;
    while more {
        cursor = cursor
            .checked_add(METHOD_ALIGNMENT - 1)
            .map(|value| value & !(METHOD_ALIGNMENT - 1))
            .context("IL section alignment overflow")?;
        let header = image.read_rva(cursor, EH_HEADER_BYTES, "IL exception header")?;
        let kind = header[0];
        ensure!(
            kind & EH_KIND_MASK == EH_TABLE,
            "unsupported IL data section"
        );
        let (size, clause_width) = if kind & EH_FAT == 0 {
            ensure!(header[2..] == [0, 0], "nonzero IL exception header padding");
            (usize::from(header[1]), EH_SMALL_CLAUSE_BYTES)
        } else {
            (
                (sbom_read_u32(header, 0, "IL section size")? >> u8::BITS) as usize,
                EH_FAT_CLAUSE_BYTES,
            )
        };
        ensure!(
            size >= EH_HEADER_BYTES && (size - EH_HEADER_BYTES).is_multiple_of(clause_width),
            "invalid IL exception section size"
        );
        frame(
            output,
            image.read_rva(cursor, size, "IL exception section")?,
        );
        cursor = cursor
            .checked_add(size)
            .context("IL exception RVA overflow")?;
        more = kind & EH_MORE_SECTIONS != 0;
    }
    frame(output, &[]);
    Ok(())
}

pub(super) fn normalize(
    image: &ManagedImage<'_>,
    bytes: &[u8],
    blobs: &[u8],
) -> Result<Normalized> {
    let tables = Tables::parse(bytes)?;
    let mut normalized = Normalized {
        tables: bytes.to_vec(),
        content: Vec::new(),
    };
    let flags = sbom_read_u32(
        image.bytes,
        image.clr_offset + CLR_FLAGS_OFFSET,
        "CLR flags",
    )?;
    ensure!(
        flags & CLR_NATIVE_ENTRY_POINT == 0,
        "native CLR entry points are unsupported"
    );
    frame(
        &mut normalized.content,
        &image.bytes[image.clr_offset + CLR_ENTRY_POINT_OFFSET
            ..image.clr_offset + CLR_ENTRY_POINT_OFFSET + size_of::<u32>()],
    );
    for index in 0..tables.rows[METHOD_DEF] as usize {
        let row = tables.row(METHOD_DEF, index)?;
        let rva = sbom_read_u32(row, 0, "MethodDef RVA")? as usize;
        normalized.content.push(u8::from(rva != 0));
        if rva != 0 {
            ensure!(
                sbom_read_u16(row, METHOD_IMPL_FLAGS_OFFSET, "method implementation flags")?
                    .is_multiple_of(METHOD_CODE_TYPE_BASE),
                "unsupported non-IL MethodDef body"
            );
            method_content(image, rva, &mut normalized.content)
                .with_context(|| format!("MethodDef row {}", index + 1))?;
        }
        let start = tables.offsets[METHOD_DEF] + index * tables.widths[METHOD_DEF];
        normalized.tables[start..start + size_of::<u32>()].fill(0);
    }
    let classes = tables.class_sizes()?;
    let mut seen_fields = BTreeSet::new();
    for index in 0..tables.rows[FIELD_RVA] as usize {
        let row = tables.row(FIELD_RVA, index)?;
        let rva = sbom_read_u32(row, 0, "FieldRVA")? as usize;
        let field = Tables::index(row, size_of::<u32>(), tables.index_width(FIELD))?;
        ensure!(
            rva != 0 && seen_fields.insert(field),
            "zero or duplicate FieldRVA"
        );
        let size = tables.field_size(field, blobs, &classes)?;
        frame(
            &mut normalized.content,
            image.read_rva(rva, size, "initialized field data")?,
        );
        let start = tables.offsets[FIELD_RVA] + index * tables.widths[FIELD_RVA];
        normalized.tables[start..start + size_of::<u32>()].fill(0);
    }
    let resource_rva = sbom_read_u32(
        image.bytes,
        image.clr_offset + CLR_RESOURCES_OFFSET,
        "managed resources RVA",
    )? as usize;
    let resource_size = sbom_read_u32(
        image.bytes,
        image.clr_offset + CLR_RESOURCES_SIZE_OFFSET,
        "managed resources size",
    )? as usize;
    ensure!(
        (resource_rva == 0) == (resource_size == 0),
        "inconsistent managed resources directory"
    );
    let resources = if resource_size == 0 {
        &[]
    } else {
        image.read_rva(resource_rva, resource_size, "managed resources")?
    };
    frame(&mut normalized.content, resources);
    Ok(normalized)
}

#[cfg(kani)]
#[path = "managed_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_extents_reject_truncation_overflow_and_unknown_layouts() {
        let mut bytes = vec![0; 28 + 14];
        bytes[4] = 2;
        bytes[7] = 0x0a;
        bytes[8..16].copy_from_slice(&(1_u64 << METHOD_DEF).to_le_bytes());
        bytes[24..28].copy_from_slice(&1_u32.to_le_bytes());
        let tables = Tables::parse(&bytes).unwrap();
        assert_eq!(tables.row(METHOD_DEF, 0).unwrap().len(), 14);
        assert!(tables.row(METHOD_DEF, 1).is_err());
        assert!(Tables::parse(&bytes[..bytes.len() - 1]).is_err());
        bytes[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Tables::parse(&bytes).is_err());
        bytes[8..16].copy_from_slice(&(1_u64 << 45).to_le_bytes());
        assert!(Tables::parse(&bytes).is_err());
        bytes[8..16].fill(0);
        bytes[6] = 0x80;
        assert!(Tables::parse(&bytes).is_err());
        assert_eq!(checked_table_range(5, 3, 2, 11), Some(5..11));
        assert!(checked_table_range(5, 3, 2, 10).is_none());
        assert!(checked_table_range(usize::MAX, 1, 1, usize::MAX).is_none());
        assert!(checked_table_range(0, usize::MAX, 2, usize::MAX).is_none());
        assert!(checked_table_range(0, 0, 0, 0).is_none());
    }

    #[test]
    fn table_and_coded_index_widths_follow_their_exact_boundaries() {
        let bytes = [0; 24];
        let mut tables = Tables::parse(&bytes).unwrap();
        assert_eq!(tables.row_width(METHOD_DEF, 2).unwrap(), 14);
        tables.rows[8] = 0xffff;
        assert_eq!(tables.index_width(8), 2);
        tables.rows[8] += 1;
        assert_eq!(tables.index_width(8), 4);
        assert_eq!(tables.row_width(METHOD_DEF, 2).unwrap(), 16);
        tables.rows[2] = 0x3fff;
        assert_eq!(tables.coded_width(2, &[2, 1, 27]), 2);
        tables.rows[2] += 1;
        assert_eq!(tables.coded_width(2, &[2, 1, 27]), 4);
    }

    #[test]
    fn signature_lengths_reject_truncation_and_noncanonical_encodings() {
        for (bytes, expected) in [
            (vec![0], 0),
            (vec![0x7f], 0x7f),
            (vec![0x80, 0x80], 0x80),
            (vec![0xbf, 0xff], 0x3fff),
            (vec![0xc0, 0, 0x40, 0], 0x4000),
            (vec![0xdf, 0xff, 0xff, 0xff], 0x1fff_ffff),
        ] {
            let mut input = bytes.as_slice();
            assert_eq!(compressed(&mut input).unwrap(), expected);
            assert_eq!(input, b"");
        }
        for bytes in [
            vec![],
            vec![0x80],
            vec![0xc0, 0, 0x40],
            vec![0x80, 0x7f],
            vec![0xc0, 0, 0, 0x80],
            vec![0xe0],
        ] {
            assert!(compressed(&mut bytes.as_slice()).is_err(), "{bytes:x?}");
        }
    }

    #[test]
    fn initialized_value_types_require_an_explicit_nonzero_local_size() {
        let mut bytes = vec![0; 74];
        let valid = [2, FIELD, CLASS_LAYOUT, FIELD_RVA]
            .into_iter()
            .fold(0_u64, |mask, table| mask | (1 << table));
        bytes[8..16].copy_from_slice(&valid.to_le_bytes());
        for offset in [24, 28, 32, 36] {
            bytes[offset..offset + 4].copy_from_slice(&1_u32.to_le_bytes());
        }
        bytes[54..56].copy_from_slice(&0x110_u16.to_le_bytes());
        bytes[58..60].copy_from_slice(&1_u16.to_le_bytes());
        bytes[62..66].copy_from_slice(&12_u32.to_le_bytes());
        bytes[66..68].copy_from_slice(&1_u16.to_le_bytes());
        let tables = Tables::parse(&bytes).unwrap();
        let sizes = tables.class_sizes().unwrap();
        assert_eq!(
            tables.field_size(1, &[0, 3, 6, 0x11, 4], &sizes).unwrap(),
            12
        );
        assert!(tables.field_size(0, &[0, 3, 6, 0x11, 4], &sizes).is_err());
        assert!(tables.field_size(2, &[0, 3, 6, 0x11, 4], &sizes).is_err());
        assert!(tables.field_size(1, &[0, 3, 6, 0x11, 5], &sizes).is_err());
        assert!(tables
            .field_size(1, &[0, 3, 6, 0x11, 4], &BTreeMap::new())
            .is_err());
        bytes[62..66].fill(0);
        let tables = Tables::parse(&bytes).unwrap();
        assert!(tables
            .field_size(1, &[0, 3, 6, 0x11, 4], &tables.class_sizes().unwrap())
            .is_err());
    }
}
