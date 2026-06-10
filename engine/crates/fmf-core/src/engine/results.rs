use std::sync::Arc;

use crate::index::{EntryId, VolumeIndex, flags};

use super::EngineError;
use super::volume::VolumeSlot;

/// One row handed across the FFI: everything the UI list needs.
pub struct Row {
    pub entry_ref: u64,
    pub frn: u64,
    pub size: u64,
    pub mtime: i64,
    pub flags: u32,
    pub name: Vec<u8>,
    pub parent_path: Vec<u8>,
}

/// Materialized, sort-ordered result. Pages are O(1) slices; reads stay
/// valid across content mutations and fail with `Stale` only after a
/// structural change (compaction/rescan).
pub struct ResultSet {
    pub(super) slots: Vec<Arc<VolumeSlot>>,
    pub(super) structural: Vec<u64>,
    pub(super) rows: Vec<(u32, EntryId)>,
}

impl ResultSet {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

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
            let mut parent_path = Vec::new();
            idx.append_parent_path(id, &mut parent_path);
            out.push(Row {
                entry_ref: ((v as u64) << 32) | id as u64,
                frn: idx.frn(id),
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
    let mut f = 0u32;
    if idx.is_dir(id) {
        f |= 1;
    }
    if !idx.is_live(id) {
        f |= 2; // deleted-since-query marker for the UI
    }
    f
}

// Reuse the flags module so the constant meanings stay in one place.
const _: () = {
    assert!(flags::IS_DIR == 1);
};
