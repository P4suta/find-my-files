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
    // The folded pool is the only contiguous one; case-exact drivers sweep
    // it with a folded needle (superset — original-case match implies the
    // folded match) and the exact comparison runs as a residual.
    let pool: &[u8] = idx.lower_pool_bytes();
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
                        // A pair whose entry has since moved its name (in-
                        // place dir rename, incremental table) covers dead
                        // bytes — skip its whole region like a stale gap.
                        if idx.name_off_of(id) as usize != off {
                            let next = if k + 1 < table.len() {
                                (table.offs[k + 1] as usize).min(pool_end)
                            } else {
                                pool_end
                            };
                            pos = next.max(hit + 1) - pool_start;
                            continue;
                        }
                        let end = off + idx.name_len_of(id);
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
                        if idx.name_off_of(id) as usize != off {
                            continue; // stale pair: dead bytes
                        }
                        let name = &pool[off..off + idx.name_len_of(id)];
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

#[cfg(test)]
mod tests {
    use memchr::memmem;

    use super::*;
    use crate::index::VolumeIndexBuilder;
    use crate::index::testutil::{raw, raw_attr, u16s};

    fn sub_driver(needle: &str) -> Driver {
        Driver::Sub {
            finder: memmem::Finder::new(needle.as_bytes()).into_owned(),
            needle_len: needle.len(),
        }
    }

    /// driver_candidates over a fresh table, id-sorted for stable assertions.
    fn run(idx: &VolumeIndex, driver: &Driver, skip_excluded: bool) -> Vec<EntryId> {
        let table = OffsetTable::build(idx);
        let mut ids = driver_candidates(idx, &table, driver, skip_excluded);
        ids.sort_unstable();
        ids
    }

    #[test]
    fn hits_spanning_two_names_are_rejected() {
        // 4000 entries guarantee multi-entry sweep chunks regardless of the
        // rayon thread count, so boundary-spanning hits are actually found
        // by the finder and must be rejected by the anchor check.
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let name = u16s("abcd");
        for i in 0..4000u64 {
            b.push(raw(100 + i, 5, &name, false, i, i as i64));
        }
        let idx = b.finish();
        // "cdab" only ever occurs across an "abcd|abcd" boundary.
        assert!(run(&idx, &sub_driver("cdab"), true).is_empty());
        // Control: in-name hits return every live entry exactly once.
        assert_eq!(
            run(&idx, &sub_driver("abcd"), true),
            (1..=4000).collect::<Vec<EntryId>>()
        );
    }

    #[test]
    fn repeated_hits_inside_one_name_yield_a_single_candidate() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let name = u16s("ababab");
        b.push(raw(10, 5, &name, false, 1, 1));
        let idx = b.finish();
        let id = idx.entry_by_record(10).unwrap();
        assert_eq!(run(&idx, &sub_driver("ab"), true), vec![id]);
    }

    #[test]
    fn stale_gap_from_dir_rename_is_skipped() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (a, d, z) = (u16s("aaa.txt"), u16s("needledir"), u16s("needle_zzz"));
        b.push(raw(10, 5, &a, false, 1, 1));
        b.push(raw(20, 5, &d, true, 0, 2));
        b.push(raw(30, 5, &z, false, 1, 3));
        let mut idx = b.finish();
        let renamed = u16s("renamed");
        idx.rename_dir_in_place(20, &renamed, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32);

        let dir = idx.entry_by_record(20).unwrap();
        let zzz = idx.entry_by_record(30).unwrap();
        // The old dir name bytes are now a stale gap: hits there map to no
        // entry and must not stop the sweep from reaching later entries.
        assert_eq!(run(&idx, &sub_driver("needle"), true), vec![zzz]);
        // A needle that only occurs inside the gap yields nothing.
        assert!(run(&idx, &sub_driver("needledir"), true).is_empty());
        // The appended new name is reachable through the re-sorted table.
        assert_eq!(run(&idx, &sub_driver("renamed"), true), vec![dir]);
    }

    #[test]
    fn tombstoned_entries_are_dropped_even_on_pool_hits() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let old = u16s("aaa.txt");
        b.push(raw(10, 5, &old, false, 1, 1));
        let mut idx = b.finish();
        let first_new = idx.len() as u32;
        let renamed = u16s("bbb.txt");
        idx.upsert(&raw(10, 5, &renamed, false, 1, 2));
        idx.merge_new_into_permutations(first_new);

        // The tombstoned entry still owns its pool bytes and table slot,
        // but a hit on it must not surface.
        assert!(run(&idx, &sub_driver("aaa"), true).is_empty());
        let new_id = idx.entry_by_record(10).unwrap();
        assert_eq!(run(&idx, &sub_driver("bbb"), true), vec![new_id]);
    }

    #[test]
    fn prefix_driver_rejects_non_anchored_hits() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (a, z) = (u16s("abc.txt"), u16s("zzabc.txt"));
        b.push(raw(10, 5, &a, false, 1, 1));
        b.push(raw(20, 5, &z, false, 1, 2));
        let idx = b.finish();
        let abc = idx.entry_by_record(10).unwrap();
        let driver = Driver::Prefix {
            bytes: b"abc".to_vec(),
        };
        // "zzabc.txt" contains the needle but not at the name start.
        assert_eq!(run(&idx, &driver, true), vec![abc]);
    }

    #[test]
    fn suffixes_driver_files_only_excluded_and_multi_suffix() {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let (bld, trc, txt, old, gho) = (
            u16s("build.log"),
            u16s("trace.log"),
            u16s("notes.txt"),
            u16s("old.log"),
            u16s("ghost.log"),
        );
        b.push(raw(10, 5, &bld, true, 0, 1)); // directory named *.log
        b.push(raw(20, 5, &trc, false, 1, 2));
        b.push(raw(30, 5, &txt, false, 1, 3));
        b.push(raw(40, 5, &old, false, 1, 4));
        b.push(raw_attr(50, 5, &gho, false, true, false)); // hidden
        let mut idx = b.finish();
        idx.delete(40); // tombstoned *.log
        let dir = idx.entry_by_record(10).unwrap();
        let trace = idx.entry_by_record(20).unwrap();
        let notes = idx.entry_by_record(30).unwrap();
        let ghost = idx.entry_by_record(50).unwrap();

        let log = |files_only: bool| Driver::Suffixes {
            suffixes: vec![b".log".to_vec()],
            files_only,
        };
        // files_only drops the dir; tombstone and hidden drop implicitly.
        assert_eq!(run(&idx, &log(true), true), vec![trace]);
        // Without files_only the dir qualifies too.
        assert_eq!(run(&idx, &log(false), true), vec![dir, trace]);
        // skip_excluded=false surfaces the hidden file (tombstone still out).
        assert_eq!(run(&idx, &log(true), false), vec![trace, ghost]);
        // Multiple suffixes union within one pass.
        let multi = Driver::Suffixes {
            suffixes: vec![b".log".to_vec(), b".txt".to_vec()],
            files_only: true,
        };
        assert_eq!(run(&idx, &multi, true), vec![trace, notes]);
    }
}
