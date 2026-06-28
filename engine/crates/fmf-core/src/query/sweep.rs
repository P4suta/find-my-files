use rayon::prelude::*;

use super::compile::Driver;
use crate::index::VolumeIndex;

// ── Drivers ─────────────────────────────────────────────────────────────

/// Sweep one dictionary sub-range — `hay` is `pool[pool_start..]` covering
/// names `ks..ke` — pushing each matching `name_id` (`k`) into `out`. Generic
/// over the finder `find` and the anchor predicate `anchor` so the optimizer
/// inlines the finder and constant-folds an always-true anchor, instead of a
/// `&mut dyn FnMut` indirect call per hit. Hits arrive in increasing pool
/// order, so the `k` cursor over the gapless `dict_off` advances monotonically
/// — amortized O(1) per hit, no binary search; a hit spilling past a name's
/// end (`hit + needle_len > end`) crosses into the next name and is rejected.
#[inline]
#[allow(clippy::too_many_arguments)]
fn sweep_range<F, A>(
    out: &mut Vec<u32>,
    pool: &[u8],
    dict_off: &[u32],
    ks: usize,
    ke: usize,
    pool_start: usize,
    hay: &[u8],
    needle_len: usize,
    mut find: F,
    mut anchor: A,
) where
    F: FnMut(&[u8]) -> Option<usize>,
    A: FnMut(usize, usize, usize) -> bool,
{
    let mut pos = 0usize;
    let mut k = ks;
    while pos < hay.len() {
        let Some(rel) = find(&hay[pos..]) else { break };
        let hit = pool_start + pos + rel;
        while k + 1 < ke && (dict_off[k + 1] as usize) <= hit {
            k += 1;
        }
        let off = dict_off[k] as usize;
        let end = dict_off.get(k + 1).map_or(pool.len(), |&e| e as usize);
        if hit + needle_len <= end && anchor(hit, off, end) {
            out.push(k as u32);
            // One hit per name is enough: resume at its end.
            pos = end - pool_start;
        } else {
            pos = hit + 1 - pool_start;
        }
    }
}

/// Sweep the distinct-name dictionary and return the set of matching
/// `name_id`s as a bitset (ADR-0032). Per-entry concerns — liveness,
/// exclusion, `files_only`, and the residual/exact-case checks — are applied
/// later in the materialize walk, where the entry id (not just its name) is
/// in hand. The dictionary is gapless (names append contiguously), so a hit
/// maps to exactly one `name_id` via a monotonic cursor over `dict_off`; a
/// match spilling past a name's end (`hit + needle_len > name_end`) crosses
/// into the next name and is rejected.
pub(super) fn driver_candidates(idx: &VolumeIndex, driver: &Driver) -> Vec<u64> {
    // The folded dictionary is the only contiguous pool; case-exact drivers
    // sweep it with a folded needle (superset — original-case match implies
    // the folded match) and the exact comparison runs as a residual.
    let pool: &[u8] = idx.dict_pool_bytes();
    let dict_off = idx.dict_offs();
    let count = dict_off.len();
    let mut set = vec![0u64; count.div_ceil(64)];
    if count == 0 || pool.is_empty() {
        return set;
    }

    // Over-split so uneven hit densities still balance across threads.
    let threads = rayon::current_num_threads().max(1) * 4;
    let per = count.div_ceil(threads).max(1);
    let ranges: Vec<(usize, usize)> = (0..count)
        .step_by(per)
        .map(|s| (s, (s + per).min(count)))
        .collect();

    // Each range owns a disjoint slice of name_ids, so the matched lists
    // never overlap — concatenate, then flip the bits once.
    let matched: Vec<Vec<u32>> = ranges
        .into_par_iter()
        .map(|(ks, ke)| {
            let pool_start = dict_off[ks] as usize;
            let pool_end = if ke < count {
                dict_off[ke] as usize
            } else {
                pool.len()
            };
            let hay = &pool[pool_start..pool_end];
            let mut out: Vec<u32> = Vec::new();

            match driver {
                Driver::Sub {
                    finder, needle_len, ..
                } => {
                    // Monomorphize over the finder + an always-true anchor so
                    // the optimizer inlines `find` and folds the anchor away —
                    // no `&mut dyn FnMut` indirection per hit (the Sub anchor
                    // does no work).
                    sweep_range(
                        &mut out,
                        pool,
                        dict_off,
                        ks,
                        ke,
                        pool_start,
                        hay,
                        *needle_len,
                        |h| finder.find(h),
                        |_, _, _| true,
                    );
                }
                Driver::Prefix { bytes, .. } => {
                    let finder = memchr::memmem::Finder::new(bytes);
                    sweep_range(
                        &mut out,
                        pool,
                        dict_off,
                        ks,
                        ke,
                        pool_start,
                        hay,
                        bytes.len(),
                        |h| finder.find(h),
                        |hit, off, _| hit == off,
                    );
                }
                Driver::Suffixes { suffixes, .. } => {
                    // Anchored tails defeat memmem's rare-byte prefilter
                    // ('.' occurs in almost every name), so a sequential
                    // dict-order tail compare wins here. `files_only` is a
                    // per-entry property (a name can back both a dir and a
                    // file) and is applied in the materialize walk.
                    for k in ks..ke {
                        let off = dict_off[k] as usize;
                        let end = dict_off.get(k + 1).map_or(pool.len(), |&e| e as usize);
                        let name = &pool[off..end];
                        if suffixes.iter().any(|s| name.ends_with(s)) {
                            out.push(k as u32);
                        }
                    }
                }
                _ => unreachable!(),
            }
            out
        })
        .collect();
    for chunk in matched {
        for k in chunk {
            set[k as usize / 64] |= 1u64 << (k as usize % 64);
        }
    }
    set
}

/// Test whether `name_id` is present in a sweep result bitset.
#[inline]
pub(super) fn name_id_in_set(set: &[u64], name_id: u32) -> bool {
    set.get(name_id as usize / 64)
        .is_some_and(|w| w >> (name_id % 64) & 1 == 1)
}

#[cfg(test)]
mod tests {
    use memchr::memmem;

    use super::*;
    use crate::index::testutil::{raw, raw_attr, u16s};
    use crate::index::{EntryId, VolumeIndexBuilder};

    fn sub_driver(needle: &str) -> Driver {
        Driver::Sub {
            finder: memmem::Finder::new(needle.as_bytes()).into_owned(),
            needle_len: needle.len(),
        }
    }

    /// `driver_candidates` expanded to live entry ids (applying the per-entry
    /// liveness/exclusion/`files_only` checks the materialize walk owns),
    /// id-sorted for stable assertions.
    fn run(idx: &VolumeIndex, driver: &Driver, skip_excluded: bool) -> Vec<EntryId> {
        let set = driver_candidates(idx, driver);
        let files_only = matches!(
            driver,
            Driver::Suffixes {
                files_only: true,
                ..
            }
        );
        let mut ids: Vec<EntryId> = (0..idx.len() as u32)
            .filter(|&id| {
                name_id_in_set(&set, idx.name_id_of(id))
                    && idx.is_live(id)
                    && !(skip_excluded && idx.is_excluded(id))
                    && !(files_only && idx.is_dir(id))
            })
            .collect();
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
