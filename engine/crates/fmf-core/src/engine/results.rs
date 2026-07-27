use std::sync::Arc;

use crate::index::{EntryId, VolumeIndex};

use super::EngineError;
use super::volume::VolumeSlot;

/// One row handed across the FFI: everything the UI list needs.
pub struct Row {
    /// Stable handle: volume index in the high 32 bits, `EntryId` in the low.
    pub entry_ref: u64,
    /// NTFS File Reference Number for this entry.
    pub frn: u64,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Last-modified time, Windows FILETIME (100 ns ticks since 1601-01-01 UTC).
    pub mtime: i64,
    /// Contract row flags. Currently only bit0 (directory) is defined.
    pub flags: u32,
    /// File name, WTF-8 bytes (no path separators).
    pub name: Vec<u8>,
    /// Parent directory path, WTF-8 bytes.
    pub parent_path: Vec<u8>,
}

/// Materialized, sort-ordered result with O(1) page slices.
///
/// Reads stay valid across compatible content mutations and fail with `Stale`
/// after a structural change or when a referenced row was deleted.
pub struct ResultSet {
    pub(super) slots: Vec<Arc<VolumeSlot>>,
    pub(super) structural: Vec<u64>,
    pub(super) rows: Vec<(u32, EntryId)>,
}

impl ResultSet {
    /// Number of rows in the materialized result.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.len()
    }

    /// True when the result contains no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether this still references the structural generations from which
    /// it was materialized.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.slots.iter().enumerate().all(|(volume, slot)| {
            slot.index
                .read()
                .as_ref()
                .is_some_and(|idx| idx.structural_generation() == self.structural[volume])
        })
    }

    /// Exact presentation identity used by `QueryTrace.unchanged`.
    ///
    /// Boundary registries establish connection/engine ownership. Core also
    /// requires identical live volume slots and the complete ordered ID
    /// column, so an old structurally-stale result can never authorize an
    /// in-place refresh.
    #[must_use]
    pub fn same_ordered_ids(&self, other: &Self) -> bool {
        self.is_current()
            && other.is_current()
            && self.slots.len() == other.slots.len()
            && self
                .slots
                .iter()
                .zip(&other.slots)
                .all(|(a, b)| Arc::ptr_eq(a, b))
            && self.rows == other.rows
    }

    /// Builds the shared page representation — 56-byte contract rows plus
    /// one string blob, offsets blob-relative — the single implementation
    /// behind both the FFI `FmfPage` and the pipe `ResultPage` payload
    /// (ADR-0018). Blob layout: per row, name bytes then parent bytes, in
    /// row order (the canonical layout the golden corpus pins).
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Stale`] if the underlying index changed since
    /// this result set was produced (the handle is stale).
    pub fn fill_page(
        &self,
        offset: usize,
        count: usize,
    ) -> Result<(Vec<fmf_contract::pod::FmfRow>, Vec<u8>), EngineError> {
        self.fill_page_with_limit(
            offset,
            count,
            fmf_contract::limits::MAX_PAYLOAD_LEN as usize,
        )
    }

    pub(super) fn fill_page_with_limit(
        &self,
        offset: usize,
        count: usize,
        maximum_payload_len: usize,
    ) -> Result<(Vec<fmf_contract::pod::FmfRow>, Vec<u8>), EngineError> {
        if count > fmf_contract::limits::MAX_PAGE_ROWS as usize {
            return Err(EngineError::PageTooLarge {
                requested: count,
                maximum: fmf_contract::limits::MAX_PAGE_ROWS,
            });
        }
        let end = (offset.saturating_add(count)).min(self.rows.len());
        let start = offset.min(end);
        let row_count = end - start;
        let fixed_len = std::mem::size_of::<u32>()
            .checked_mul(2)
            .and_then(|header| {
                fmf_contract::pod::FmfRow::LEN
                    .checked_mul(row_count)
                    .and_then(|rows_len| header.checked_add(rows_len))
            })
            .ok_or(EngineError::PageEncoding("encoded page length overflowed"))?;
        if fixed_len > maximum_payload_len {
            return Err(EngineError::PageEncoding(
                "encoded page exceeds the maximum payload length",
            ));
        }

        let mut blob = Vec::new();
        let mut rows = Vec::with_capacity(row_count);
        let guards: Vec<_> = self.slots.iter().map(|slot| slot.index.read()).collect();
        for (volume, guard) in guards.iter().enumerate() {
            let index = guard.as_ref().ok_or(EngineError::Stale)?;
            if index.structural_generation() != self.structural[volume] {
                return Err(EngineError::Stale);
            }
        }

        for &(volume, id) in &self.rows[start..end] {
            let index = guards[volume as usize].as_ref().ok_or(EngineError::Stale)?;
            if !index.is_live(id) {
                return Err(EngineError::Stale);
            }
            let name = index.name(id);
            let mut parent_path = Vec::new();
            index.append_parent_path(id, &mut parent_path)?;
            let additional = name
                .len()
                .checked_add(parent_path.len())
                .ok_or(EngineError::PageEncoding("encoded page length overflowed"))?;
            let encoded_len = fixed_len
                .checked_add(blob.len())
                .and_then(|len| len.checked_add(additional))
                .ok_or(EngineError::PageEncoding("encoded page length overflowed"))?;
            if encoded_len > maximum_payload_len {
                return Err(EngineError::PageEncoding(
                    "encoded page exceeds the maximum payload length",
                ));
            }
            blob.try_reserve_exact(additional)
                .map_err(|_| EngineError::PageEncoding("could not allocate the result page"))?;

            let name_len = u32::try_from(name.len())
                .map_err(|_| EngineError::PageEncoding("file name exceeds u32 length"))?;
            let parent_path_len = u32::try_from(parent_path.len())
                .map_err(|_| EngineError::PageEncoding("parent path exceeds u32 length"))?;
            let name_off = u32::try_from(blob.len())
                .map_err(|_| EngineError::PageEncoding("string blob exceeds u32 offset"))?;
            blob.extend_from_slice(name);
            let parent_off = u32::try_from(blob.len())
                .map_err(|_| EngineError::PageEncoding("string blob exceeds u32 offset"))?;
            blob.extend_from_slice(&parent_path);
            rows.push(fmf_contract::pod::FmfRow {
                entry_ref: ((volume as u64) << 32) | id as u64,
                frn: index.frn(id).0,
                size: index.size(id),
                mtime: index.mtime(id),
                name_off,
                parent_path_off: parent_off,
                flags: idx_flags(index, id),
                name_len,
                parent_path_len,
                _reserved: 0,
            });
        }
        Ok((rows, blob))
    }

    /// Materialize `[offset, offset + count)` of the result into owned rows.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Stale`] if any backing volume index changed
    /// since this result set was produced (the handle is stale).
    pub fn page(&self, offset: usize, count: usize) -> Result<Vec<Row>, EngineError> {
        let end = (offset.saturating_add(count)).min(self.rows.len());
        let start = offset.min(end);
        let mut out = Vec::with_capacity(end - start);

        let guards: Vec<_> = self.slots.iter().map(|s| s.index.read()).collect();
        for (v, guard) in guards.iter().enumerate() {
            let idx = guard.as_ref().ok_or(EngineError::Stale)?;
            if idx.structural_generation() != self.structural[v] {
                return Err(EngineError::Stale);
            }
        }
        for &(v, id) in &self.rows[start..end] {
            let idx = guards[v as usize].as_ref().ok_or(EngineError::Stale)?;
            if !idx.is_live(id) {
                return Err(EngineError::Stale);
            }
            let mut parent_path = Vec::new();
            idx.append_parent_path(id, &mut parent_path)?;
            out.push(Row {
                entry_ref: ((v as u64) << 32) | id as u64,
                frn: idx.frn(id).0,
                size: idx.size(id),
                mtime: idx.mtime(id),
                flags: idx_flags(idx, id),
                name: idx.name(id).to_vec(),
                parent_path,
            });
        }
        Ok(out)
    }
}

fn idx_flags(idx: &VolumeIndex, id: EntryId) -> u32 {
    if idx.is_dir(id) {
        fmf_contract::pod::row_flags::DIRECTORY
    } else {
        0
    }
}
