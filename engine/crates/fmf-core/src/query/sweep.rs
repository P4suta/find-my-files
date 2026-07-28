use rayon::prelude::*;

use super::compile::Driver;
use super::{QueryCancellation, QueryCancelled};
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
    cancellation: &QueryCancellation,
    mut find: F,
    mut anchor: A,
) -> Result<(), QueryCancelled>
where
    F: FnMut(&[u8]) -> Option<usize>,
    A: FnMut(usize, usize, usize) -> bool,
{
    let mut pos = 0usize;
    let mut k = ks;
    let mut iterations = 0usize;
    while pos < hay.len() {
        if iterations.is_multiple_of(256) {
            cancellation.check()?;
        }
        iterations += 1;
        let Some(rel) = find(&hay[pos..]) else { break };
        let hit = pool_start + pos + rel;
        while k + 1 < ke && (dict_off[k + 1] as usize) <= hit {
            if k.is_multiple_of(1024) {
                cancellation.check()?;
            }
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
    cancellation.check()
}

#[inline]
fn canonical_driver_matches(driver: &Driver, name: &[u8]) -> bool {
    match driver {
        Driver::Sub { finder, .. } => finder.find(name).is_some(),
        Driver::Prefix { bytes, .. } => name.starts_with(bytes),
        Driver::Suffixes { suffixes, .. } => suffixes.iter().any(|s| name.ends_with(s)),
        Driver::FullScan | Driver::MatchAll => false,
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
#[cfg(test)]
pub(super) fn driver_candidates(idx: &VolumeIndex, driver: &Driver) -> Vec<u64> {
    driver_candidates_cancellable(idx, driver, &QueryCancellation::new())
        .expect("fresh cancellation token cannot cancel")
}

pub(super) fn driver_candidates_cancellable(
    idx: &VolumeIndex,
    driver: &Driver,
    cancellation: &QueryCancellation,
) -> Result<Vec<u64>, QueryCancelled> {
    cancellation.check()?;
    // The folded dictionary is the only contiguous pool; case-exact drivers
    // sweep it with a folded needle (superset — original-case match implies
    // the folded match) and the exact comparison runs as a residual.
    let pool: &[u8] = idx.dict_pool_bytes();
    let dict_off = idx.dict_offs();
    let count = dict_off.len();
    let mut set = vec![0u64; count.div_ceil(64)];
    if count == 0 || pool.is_empty() {
        return Ok(set);
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
    let mut matched: Vec<Vec<u32>> = ranges
        .into_par_iter()
        .map(|(ks, ke)| -> Result<_, QueryCancelled> {
            cancellation.check()?;
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
                        cancellation,
                        |h| finder.find(h),
                        |_, _, _| true,
                    )?;
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
                        cancellation,
                        |h| finder.find(h),
                        |hit, off, _| hit == off,
                    )?;
                }
                Driver::Suffixes { suffixes, .. } => {
                    // Anchored tails defeat memmem's rare-byte prefilter
                    // ('.' occurs in almost every name), so a sequential
                    // dict-order tail compare wins here. `files_only` is a
                    // per-entry property (a name can back both a dir and a
                    // file) and is applied in the materialize walk.
                    for k in ks..ke {
                        if k.is_multiple_of(1024) {
                            cancellation.check()?;
                        }
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

            if driver.canonical() {
                // The raw SIMD sweep above catches names already in the
                // query's NFC spelling. Complete the superset only for
                // non-ASCII dictionary names whose folded canonical view
                // differs; ASCII folded names are already lowercase NFC and
                // therefore cannot add a missed candidate. Scratch is reused
                // for the entire range and never becomes standing index RAM.
                let mut canonical = Vec::new();
                for k in ks..ke {
                    if k.is_multiple_of(1024) {
                        cancellation.check()?;
                    }
                    let off = dict_off[k] as usize;
                    let end = dict_off.get(k + 1).map_or(pool.len(), |&e| e as usize);
                    let name = &pool[off..end];
                    if name.is_ascii() {
                        continue;
                    }
                    crate::wtf8::normalize_wtf8_into(name, true, &mut canonical);
                    if canonical != name && canonical_driver_matches(driver, &canonical) {
                        out.push(k as u32);
                    }
                }
                out.sort_unstable();
                out.dedup();
            }
            cancellation.check()?;
            Ok(out)
        })
        .collect::<Result<_, _>>()?;

    if driver.canonical() {
        // NFC and the storage's length-preserving fold do not commute for
        // every spelling (`I` + U+0307 versus U+0130 is the minimal example).
        // The distinct-name pass above operates on the folded dictionary, so
        // complete it from original spellings for entries whose original and
        // stored-folded names differ. Fold-identical names are already covered
        // exactly by that dictionary pass; ASCII names are covered by the raw
        // sweep because ASCII folding and NFC commute.
        let entry_count = idx.len();
        let entry_per = entry_count.div_ceil(threads).max(1);
        let entry_ranges: Vec<(usize, usize)> = (0..entry_count)
            .step_by(entry_per)
            .map(|s| (s, (s + entry_per).min(entry_count)))
            .collect();
        let original_completion: Vec<Vec<u32>> = entry_ranges
            .into_par_iter()
            .map(|(start, end)| -> Result<_, QueryCancelled> {
                cancellation.check()?;
                let mut canonical = Vec::new();
                let mut out = Vec::new();
                for raw_id in start..end {
                    if raw_id.is_multiple_of(1024) {
                        cancellation.check()?;
                    }
                    let id = raw_id as u32;
                    if !idx.is_live(id) || idx.is_fold_identical(id) {
                        continue;
                    }
                    let original = idx.name(id);
                    if original.is_ascii() {
                        continue;
                    }
                    crate::wtf8::normalize_wtf8_into(original, true, &mut canonical);
                    if canonical_driver_matches(driver, &canonical) {
                        out.push(idx.name_id_of(id));
                    }
                }
                out.sort_unstable();
                out.dedup();
                cancellation.check()?;
                Ok(out)
            })
            .collect::<Result<_, _>>()?;
        matched.extend(original_completion);
    }

    for chunk in matched {
        cancellation.check()?;
        for k in chunk {
            set[k as usize / 64] |= 1u64 << (k as usize % 64);
        }
    }
    cancellation.check()?;
    Ok(set)
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
            canonical: false,
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
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
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
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let name = u16s("ababab");
        b.push(raw(10, 5, &name, false, 1, 1));
        let idx = b.finish();
        let id = idx.entry_by_record(10).unwrap();
        assert_eq!(run(&idx, &sub_driver("ab"), true), vec![id]);
    }

    #[test]
    fn stale_gap_from_dir_rename_is_skipped() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let (a, d, z) = (u16s("aaa.txt"), u16s("needledir"), u16s("needle_zzz"));
        b.push(raw(10, 5, &a, false, 1, 1));
        b.push(raw(20, 5, &d, true, 0, 2));
        b.push(raw(30, 5, &z, false, 1, 3));
        let mut idx = b.finish();
        let renamed = u16s("renamed");
        idx.rename_dir_synthetic_in_place(20, &renamed, 5).unwrap();
        idx.merge_new_into_permutations(idx.len() as u32)
            .expect("fixture topology remains valid");
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
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let old = u16s("aaa.txt");
        b.push(raw(10, 5, &old, false, 1, 1));
        let mut idx = b.finish();
        let first_new = idx.len() as u32;
        let renamed = u16s("bbb.txt");
        idx.upsert_synthetic(&raw(10, 5, &renamed, false, 1, 2));
        idx.merge_new_into_permutations(first_new)
            .expect("fixture topology remains valid");
        // The tombstoned entry still owns its pool bytes and table slot,
        // but a hit on it must not surface.
        assert!(run(&idx, &sub_driver("aaa"), true).is_empty());
        let new_id = idx.entry_by_record(10).unwrap();
        assert_eq!(run(&idx, &sub_driver("bbb"), true), vec![new_id]);
    }

    #[test]
    fn prefix_driver_rejects_non_anchored_hits() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let (a, z) = (u16s("abc.txt"), u16s("zzabc.txt"));
        b.push(raw(10, 5, &a, false, 1, 1));
        b.push(raw(20, 5, &z, false, 1, 2));
        let idx = b.finish();
        let abc = idx.entry_by_record(10).unwrap();
        let driver = Driver::Prefix {
            bytes: b"abc".to_vec(),
            canonical: false,
        };
        // "zzabc.txt" contains the needle but not at the name start.
        assert_eq!(run(&idx, &driver, true), vec![abc]);
    }

    #[test]
    fn suffixes_driver_files_only_excluded_and_multi_suffix() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
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
            canonical: false,
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
            canonical: false,
        };
        assert_eq!(run(&idx, &multi, true), vec![trace, notes]);
    }

    #[test]
    fn canonical_driver_unions_raw_and_decomposed_dictionary_names() {
        let mut b = VolumeIndexBuilder::new_synthetic("C:", 5);
        let nfc = u16s("café.txt");
        let nfd = u16s("cafe\u{301}.txt");
        let plain = u16s("cafe.txt");
        let non_commuting = u16s("I\u{307}stanbul.txt");
        b.push(raw(10, 5, &nfc, false, 1, 1));
        b.push(raw(20, 5, &nfd, false, 1, 2));
        b.push(raw(30, 5, &plain, false, 1, 3));
        b.push(raw(40, 5, &non_commuting, false, 1, 4));
        let idx = b.finish();
        let driver = Driver::Sub {
            finder: memmem::Finder::new("é".as_bytes()).into_owned(),
            needle_len: "é".len(),
            canonical: true,
        };
        assert_eq!(
            run(&idx, &driver, true),
            vec![
                idx.entry_by_record(10).unwrap(),
                idx.entry_by_record(20).unwrap()
            ]
        );

        let driver = Driver::Sub {
            finder: memmem::Finder::new("İ".as_bytes()).into_owned(),
            needle_len: "İ".len(),
            canonical: true,
        };
        assert_eq!(
            run(&idx, &driver, true),
            vec![idx.entry_by_record(40).unwrap()],
            "original-spelling completion must cover NFC/fold non-commutation"
        );
    }
}
