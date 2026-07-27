use std::sync::Arc;

use crate::index::{EntryId, SortKey, VolumeIndex};
use crate::metrics::QueryTrace;
use crate::query::{self, QueryOptions};

use super::volume::{VolumeQueryCache, VolumeSlot};
use super::{Engine, EngineError, QueryCancellation, ResultSet, VolumeState};

/// Kill switch for the incremental query cache (`FMF_QUERY_CACHE=0`) — if a
/// subsumption bug ever surfaces in the field, users get correctness back
/// without a rebuild.
fn query_cache_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FMF_QUERY_CACHE").map_or(true, |v| v != "0"))
}

impl Engine {
    /// Run a query against every Ready volume and merge the per-volume,
    /// already-sorted id lists into one ordered result set.
    ///
    /// Per volume, the previous result is kept (`VolumeSlot::last_query`);
    /// when the new query provably narrows it and the index generation is
    /// unchanged, the candidate set is the previous hits instead of the
    /// whole index (`query::refine`) — typing one more letter costs
    /// O(previous hits), not O(index).
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::QueryTooLong`] when `text` exceeds the contract
    /// bound, [`EngineError::Parse`] if it is not valid,
    /// [`EngineError::Compile`] if a valid query fails to compile (e.g. a bad
    /// regex term), or [`EngineError::Stale`] if a volume loses its index
    /// before the result can be merged.
    pub fn query(
        &self,
        text: &str,
        opt: &QueryOptions,
    ) -> Result<(ResultSet, QueryTrace), EngineError> {
        self.query_cancellable(text, opt, &QueryCancellation::new(), None)
    }

    /// Run a query with cooperative cancellation and an optional explicitly
    /// validated currently-presented result.
    ///
    /// `presentation_basis` is used only for exact ordered-ID equality after
    /// the new result is complete. Boundaries must first prove that the basis
    /// handle is live and belongs to the same engine/connection.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Cancelled`] when cancellation is observed,
    /// [`EngineError::QueryTooLong`] when `text` exceeds the contract bound,
    /// [`EngineError::Parse`] or [`EngineError::Compile`] for invalid query
    /// text, or [`EngineError::Stale`] if a volume loses its index before the
    /// result can be merged.
    pub fn query_cancellable(
        &self,
        text: &str,
        opt: &QueryOptions,
        cancellation: &QueryCancellation,
        presentation_basis: Option<&ResultSet>,
    ) -> Result<(ResultSet, QueryTrace), EngineError> {
        cancellation.check()?;
        if text.len() > fmf_contract::limits::MAX_QUERY_BYTES as usize {
            return Err(EngineError::QueryTooLong {
                actual: text.len(),
                maximum: fmf_contract::limits::MAX_QUERY_BYTES,
            });
        }
        let mut trace = QueryTrace {
            query_length: text.chars().count() as u32,
            ..Default::default()
        };
        let t_total = crate::metrics::Stage::start();
        let mut stage = crate::metrics::Stage::start();

        // Reuse the previous compile when the same (text, case) is re-issued
        // (USN-driven requery / RefreshInPlace) — parse + compile are skipped,
        // which is the biggest single cost for a regex query.
        let cached = {
            let cache = self.compile_cache.lock();
            cache
                .as_ref()
                .and_then(|(t, o, q)| (t.as_str() == text && o == opt).then(|| Arc::clone(q)))
        };
        cancellation.check()?;
        let compiled = if let Some(q) = cached {
            trace.parse_us = stage.lap();
            q
        } else if opt.regex_mode {
            // Whole-query regex: the entire text is one pattern — no parse,
            // no operators (ADR-0023).
            trace.parse_us = stage.lap();
            let q = Arc::new(query::compile_whole_regex(text, opt.case, opt.regex_scope)?);
            cancellation.check()?;
            *self.compile_cache.lock() = Some((text.to_string(), *opt, Arc::clone(&q)));
            q
        } else {
            let ast = query::parse(text)?;
            cancellation.check()?;
            trace.parse_us = stage.lap();
            let q = Arc::new(query::compile(&ast, opt.case, &date_resolver())?);
            cancellation.check()?;
            *self.compile_cache.lock() = Some((text.to_string(), *opt, Arc::clone(&q)));
            q
        };
        cancellation.check()?;
        trace.driver = compiled.driver_label();
        trace.compile_us = stage.lap();

        let slots: Vec<Arc<VolumeSlot>> = self
            .volumes
            .read()
            .iter()
            .filter(|s| *s.phase.lock() == VolumeState::Ready)
            .cloned()
            .collect();
        cancellation.check()?;

        let mut per_volume: Vec<(Arc<VolumeSlot>, Arc<[EntryId]>, u64)> = Vec::new();
        let mut pending_caches: Vec<(Arc<VolumeSlot>, VolumeQueryCache)> = Vec::new();
        let mut refined = 0usize;
        for slot in &slots {
            cancellation.check()?;
            let guard = slot.index.read();
            let Some(idx) = guard.as_ref() else { continue };
            let cache = slot.last_query.lock();
            let prev_ids = if query_cache_enabled() {
                cache.as_ref().and_then(|c| {
                    (c.content_generation == idx.content_generation()
                        && c.structural_generation == idx.structural_generation()
                        && query::subsumes(&c.compiled, &c.opt, &compiled, opt))
                    .then(|| c.ids.clone())
                })
            } else {
                None
            };
            drop(cache);
            let (r, m) = match &prev_ids {
                Some(ids) => {
                    refined += 1;
                    query::refine_cancellable(idx, &compiled, opt, ids, cancellation)?
                }
                None => query::search_cancellable(idx, &compiled, opt, cancellation)?,
            };
            cancellation.check()?;
            trace.memo_us += m.memo_us;
            trace.scan_us += m.scan_us;
            trace.materialize_us += m.materialize_us;
            trace.entries_scanned += m.entries_scanned;
            trace.excluded_skipped += m.excluded_skipped;
            let ids: Arc<[EntryId]> = Arc::from(r.ids);
            pending_caches.push((
                slot.clone(),
                VolumeQueryCache {
                    compiled: compiled.clone(),
                    opt: *opt,
                    content_generation: r.content_generation,
                    structural_generation: r.structural_generation,
                    ids: ids.clone(),
                },
            ));
            per_volume.push((slot.clone(), ids, r.structural_generation));
        }
        trace.volumes = per_volume.len() as u32;
        trace.cache = if refined == 0 {
            "miss"
        } else if refined == per_volume.len() {
            "refine"
        } else {
            "partial"
        }
        .to_string();
        stage.lap();

        // K-way merge by the sort key (typically 1-3 volumes). One volume —
        // the common setup — is a straight copy; the cursor merge below
        // costs more than the whole scan for large result sets.
        let total: usize = per_volume.iter().map(|(_, ids, _)| ids.len()).sum();
        let mut rows: Vec<(u32, EntryId)> = Vec::with_capacity(total);
        if let [(_, ids, _)] = per_volume.as_slice() {
            for (position, &id) in ids.iter().enumerate() {
                if position.is_multiple_of(1024) {
                    cancellation.check()?;
                }
                rows.push((0, id));
            }
        } else {
            let guards: Vec<_> = per_volume
                .iter()
                .map(|(slot, _, _)| slot.index.read())
                .collect();
            let indices = guards
                .iter()
                .map(|guard| guard.as_ref().ok_or(EngineError::Stale))
                .collect::<Result<Vec<_>, _>>()?;
            for (index, (_, _, expected_structural)) in indices.iter().zip(&per_volume) {
                validate_merge_index(index, *expected_structural)?;
            }
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
                            let idx_b = indices[vb];
                            let idx_v = indices[vv];
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
                        if rows.len().is_multiple_of(1024) {
                            cancellation.check()?;
                        }
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
        cancellation.check()?;

        let result = ResultSet {
            slots: per_volume.iter().map(|(s, _, _)| s.clone()).collect(),
            structural: per_volume.iter().map(|(_, _, g)| *g).collect(),
            rows,
        };
        trace.unchanged = presentation_basis.is_some_and(|basis| result.same_ordered_ids(basis));

        // Refinement caches are published as one transaction only after
        // every volume and the merge completed without observing
        // cancellation. They are an accelerator, never presentation state.
        for (slot, cache) in pending_caches {
            *slot.last_query.lock() = Some(cache);
        }
        self.metrics.record_query(trace.clone());

        // The per-query observability line is emitted by the transport layer
        // (fmf_core::diag::log_query_served), not here: that is where the
        // result handle `rid` and the ambient `qid` span both exist, so one
        // line carries the full correlation. Direct callers (CLI/tests) of
        // Engine::query simply produce no "query served" line.

        Ok((result, trace))
    }
}

/// The id arrays and their generation are captured while the old index read
/// guard is held. A full rescan can replace that index before the merge
/// reacquires all guards; reject that snapshot before any old id indexes the
/// replacement.
const fn validate_merge_index(
    index: &VolumeIndex,
    expected_structural: u64,
) -> Result<(), EngineError> {
    if index.structural_generation() == expected_structural {
        Ok(())
    } else {
        Err(EngineError::Stale)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Frn, RawEntry, VolumeIndexBuilder};

    #[test]
    fn merge_rejects_a_rebuilt_index_before_dereferencing_old_ids() {
        let mut old_builder = VolumeIndexBuilder::new_synthetic("C:", 5);
        let name = "old.txt".encode_utf16().collect::<Vec<_>>();
        old_builder.push(RawEntry {
            parent_frn: Frn(5),
            frn: Frn(10),
            name_utf16: &name,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: 1,
        });
        let old = old_builder.finish();
        let captured_structural = old.structural_generation();
        let old_id = old.len() as EntryId - 1;

        let mut replacement = VolumeIndexBuilder::new_synthetic("C:", 5).finish();
        replacement.bump_structural_from(captured_structural);
        assert!(
            old_id as usize >= replacement.len(),
            "the captured id would index past this replacement"
        );

        assert!(matches!(
            validate_merge_index(&replacement, captured_structural),
            Err(EngineError::Stale)
        ));
    }
}
