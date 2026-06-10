use std::sync::Arc;

use crate::index::{EntryId, SortKey, VolumeIndex};
use crate::metrics::QueryTrace;
use crate::query::{self, QueryOptions};

use super::volume::VolumeSlot;
use super::{Engine, EngineError, ResultSet, VolumePhase};

impl Engine {
    /// Run a query against every Ready volume and merge the per-volume,
    /// already-sorted id lists into one ordered result set.
    pub fn query(
        &self,
        text: &str,
        opt: &QueryOptions,
    ) -> Result<(ResultSet, QueryTrace), EngineError> {
        let mut trace = QueryTrace {
            query: text.to_string(),
            driver: "full-scan".to_string(),
            ..Default::default()
        };
        let t_total = crate::metrics::Stage::start();
        let mut stage = crate::metrics::Stage::start();

        let ast = query::parse(text)?;
        trace.parse_us = stage.lap();
        let compiled = query::compile(&ast, opt.case, &date_resolver())?;
        trace.compile_us = stage.lap();

        let slots: Vec<Arc<VolumeSlot>> = self
            .volumes
            .read()
            .iter()
            .filter(|s| *s.phase.lock() == VolumePhase::Ready)
            .cloned()
            .collect();

        let mut per_volume: Vec<(Arc<VolumeSlot>, Vec<EntryId>, u64)> = Vec::new();
        for slot in &slots {
            let guard = slot.index.read();
            let Some(idx) = guard.as_ref() else { continue };
            let (r, m) = query::search(idx, &compiled, opt);
            trace.memo_us += m.memo_us;
            trace.scan_us += m.scan_us;
            trace.materialize_us += m.materialize_us;
            trace.entries_scanned += m.entries_scanned;
            trace.excluded_skipped += m.excluded_skipped;
            per_volume.push((slot.clone(), r.ids, r.structural_generation));
        }
        trace.volumes = per_volume.len() as u32;
        stage.lap();

        // K-way merge by the sort key (typically 1-3 volumes).
        let total: usize = per_volume.iter().map(|(_, ids, _)| ids.len()).sum();
        let mut rows: Vec<(u32, EntryId)> = Vec::with_capacity(total);
        {
            let guards: Vec<_> = per_volume
                .iter()
                .map(|(slot, _, _)| slot.index.read())
                .collect();
            let mut cursors: Vec<usize> = vec![0; per_volume.len()];
            loop {
                let mut best: Option<usize> = None;
                for (v, (_, ids, _)) in per_volume.iter().enumerate() {
                    if cursors[v] >= ids.len() {
                        continue;
                    }
                    best = match best {
                        None => Some(v),
                        Some(b) => {
                            let (ib, vb) = (per_volume[b].1[cursors[b]], b);
                            let (iv, vv) = (ids[cursors[v]], v);
                            let idx_b = guards[vb].as_ref().unwrap();
                            let idx_v = guards[vv].as_ref().unwrap();
                            if cmp_entries(idx_v, iv, idx_b, ib, opt) == std::cmp::Ordering::Less {
                                Some(vv)
                            } else {
                                Some(vb)
                            }
                        }
                    };
                }
                match best {
                    Some(v) => {
                        rows.push((v as u32, per_volume[v].1[cursors[v]]));
                        cursors[v] += 1;
                    }
                    None => break,
                }
            }
        }

        trace.merge_us = stage.lap();
        trace.hits = rows.len() as u64;
        trace.total_us = t_total.elapsed_us();
        self.metrics.record_query(trace.clone());

        Ok((
            ResultSet {
                slots: per_volume.iter().map(|(s, _, _)| s.clone()).collect(),
                structural: per_volume.iter().map(|(_, _, g)| *g).collect(),
                rows,
            },
            trace,
        ))
    }
}

fn cmp_entries(
    a_idx: &VolumeIndex,
    a: EntryId,
    b_idx: &VolumeIndex,
    b: EntryId,
    opt: &QueryOptions,
) -> std::cmp::Ordering {
    let ord = match opt.sort {
        SortKey::Name => a_idx.lower_name(a).cmp(b_idx.lower_name(b)),
        SortKey::Size => a_idx.size(a).cmp(&b_idx.size(b)),
        SortKey::Mtime => a_idx.mtime(a).cmp(&b_idx.mtime(b)),
    };
    if opt.desc { ord.reverse() } else { ord }
}

#[cfg(windows)]
fn date_resolver() -> impl query::DateResolver {
    query::WindowsLocalResolver
}
#[cfg(not(windows))]
fn date_resolver() -> impl query::DateResolver {
    query::UtcResolver
}
