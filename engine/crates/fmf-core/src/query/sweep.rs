use rayon::prelude::*;

use super::compile::Driver;
use super::memo::OffsetTable;
use crate::index::{EntryId, VolumeIndex};

// ── Drivers ─────────────────────────────────────────────────────────────

/// Run a pool-sweep driver and return live (and, when filtering, non-
/// excluded) candidate entries. Hits are validated against entry
/// boundaries: pool bytes from renamed-away names ("stale gaps") and
/// matches spanning two names map outside their entry's range and are
/// rejected.
pub(super) fn driver_candidates(
    idx: &VolumeIndex,
    table: &OffsetTable,
    driver: &Driver,
    skip_excluded: bool,
) -> Vec<EntryId> {
    let folded = match driver {
        Driver::Sub { folded, .. }
        | Driver::Prefix { folded, .. }
        | Driver::Suffixes { folded, .. } => *folded,
        _ => unreachable!("non-sweep driver"),
    };
    let pool: &[u8] = if folded {
        idx.lower_pool_bytes()
    } else {
        idx.name_pool_bytes()
    };
    if table.len() == 0 || pool.is_empty() {
        return Vec::new();
    }

    // Over-split so uneven hit densities still balance across threads.
    let threads = rayon::current_num_threads().max(1) * 4;
    let per = table.len().div_ceil(threads).max(1);
    let ranges: Vec<(usize, usize)> = (0..table.len())
        .step_by(per)
        .map(|s| (s, (s + per).min(table.len())))
        .collect();

    let accept = |id: EntryId| idx.is_live(id) && !(skip_excluded && idx.is_excluded(id));

    let chunks: Vec<Vec<EntryId>> = ranges
        .into_par_iter()
        .map(|(ks, ke)| {
            let pool_start = table.offs[ks] as usize;
            let pool_end = if ke < table.len() {
                table.offs[ke] as usize
            } else {
                pool.len()
            };
            let hay = &pool[pool_start..pool_end];
            let mut out = Vec::new();

            let mut sweep =
                |needle_len: usize,
                 find: &mut dyn FnMut(&[u8]) -> Option<usize>,
                 anchor: &mut dyn FnMut(usize, usize, usize) -> bool| {
                    let mut pos = 0usize;
                    // Hits arrive in increasing pool order, so the entry
                    // cursor advances monotonically — amortized O(1) per hit
                    // instead of a binary search.
                    let mut k = ks;
                    while pos < hay.len() {
                        let Some(rel) = find(&hay[pos..]) else { break };
                        let hit = pool_start + pos + rel;
                        while k + 1 < ke && (table.offs[k + 1] as usize) <= hit {
                            k += 1;
                        }
                        let id = table.ids[k];
                        let off = table.offs[k] as usize;
                        let end = off + idx.name(id).len();
                        if hit + needle_len <= end && anchor(hit, off, end) {
                            if accept(id) {
                                out.push(id);
                            }
                            // One hit per entry is enough: resume at its end.
                            pos = end - pool_start;
                        } else if hit >= end {
                            // Stale gap between entries: jump to the next entry.
                            let next = if k + 1 < table.len() {
                                (table.offs[k + 1] as usize).min(pool_end)
                            } else {
                                pool_end
                            };
                            pos = next.max(hit + 1) - pool_start;
                        } else {
                            pos = hit + 1 - pool_start;
                        }
                    }
                };

            match driver {
                Driver::Sub {
                    finder, needle_len, ..
                } => {
                    sweep(*needle_len, &mut |h| finder.find(h), &mut |_, _, _| true);
                }
                Driver::Prefix { bytes, .. } => {
                    let finder = memchr::memmem::Finder::new(bytes);
                    sweep(bytes.len(), &mut |h| finder.find(h), &mut |hit, off, _| {
                        hit == off
                    });
                }
                Driver::Suffixes {
                    suffixes,
                    files_only,
                    ..
                } => {
                    // Anchored tails defeat memmem's rare-byte prefilter
                    // ('.' occurs in almost every name), so a sequential
                    // table-order tail compare wins here.
                    for k in ks..ke {
                        let id = table.ids[k];
                        let off = table.offs[k] as usize;
                        let name = &pool[off..off + idx.name(id).len()];
                        if suffixes.iter().any(|s| name.ends_with(s))
                            && (!*files_only || !idx.is_dir(id))
                            && accept(id)
                        {
                            out.push(id);
                        }
                    }
                }
                _ => unreachable!(),
            }
            out
        })
        .collect();
    chunks.concat()
}
