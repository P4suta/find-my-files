//! Reduce a journal batch to per-object final operations and apply them to the
//! link-row index.
//!
//! Reason flags are aggregated per FRN first (a rename storm touching one
//! file collapses to one final state — docs/ARCHITECTURE.md), then ops run in
//! first-touch order so that `mkdir a; touch a\b` resolves parents. Hard-link
//! changes reconcile an authoritative complete set; one event name is never
//! mistaken for the object's only path.

use std::collections::HashMap;

use rustc_hash::FxHashMap;

use super::records::{UsnRecord, reason};
use crate::index::{Frn, LinkReconcileStats, RawEntry, VolumeIndex};

/// One searchable directory link returned by a complete live metadata read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkInfo {
    /// Complete record+sequence reference of the parent directory.
    pub parent_frn: u64,
    /// Original NTFS name, preserving arbitrary UTF-16 code units.
    pub name: Vec<u16>,
}

/// Authoritative outcome of reading one file object's current link set.
///
/// `Gone` is intentionally distinct from `Failed`: only an exact OS-level
/// proof that the requested object generation no longer exists permits the
/// index to discard every known path. A malformed record, incomplete
/// `$ATTRIBUTE_LIST`, or transient I/O failure instead rejects the journal
/// batch and requests a clean rescan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkSnapshot {
    /// Complete, non-empty set of searchable links.
    Present(Vec<LinkInfo>),
    /// The exact full-FRN object generation is proven not to exist.
    Gone,
    /// No authoritative answer is available.
    Failed,
}

/// Concrete metadata source for created/changed files.
///
/// The USN record carries neither size/mtime nor a complete hard-link set
/// (RESEARCH.md): production reads the current NTFS volume, while deterministic
/// replays select none, one constant, or FRN-keyed fixture maps.
///
/// This deliberately is not a trait/port. The engine's two OS seams remain
/// exactly `SnapshotStore` and `JournalSource` (ADR-0018).
pub struct MetadataSource {
    kind: MetadataSourceKind,
}

enum MetadataSourceKind {
    None,
    Constant((u64, i64)),
    Map {
        stats: HashMap<u64, (u64, i64)>,
        links: HashMap<u64, LinkSnapshot>,
    },
    #[cfg(windows)]
    Volume(super::session::VolumeMetadataFetcher),
}

impl MetadataSource {
    /// A source with no answer; values are carried over or initialized to 0.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            kind: MetadataSourceKind::None,
        }
    }

    /// Return the same size/mtime for every FRN (deterministic tests).
    #[must_use]
    pub const fn constant(size: u64, mtime: i64) -> Self {
        Self {
            kind: MetadataSourceKind::Constant((size, mtime)),
        }
    }

    /// Return canned size/mtime values keyed by full FRN (fixture replays).
    #[must_use]
    pub fn map(values: HashMap<u64, (u64, i64)>) -> Self {
        Self {
            kind: MetadataSourceKind::Map {
                stats: values,
                links: HashMap::new(),
            },
        }
    }

    /// Return canned metadata and complete hard-link sets keyed by full FRN.
    #[must_use]
    pub fn map_with_links(
        stats: HashMap<u64, (u64, i64)>,
        links: HashMap<u64, Vec<LinkInfo>>,
    ) -> Self {
        let links = links
            .into_iter()
            .map(|(frn, links)| {
                let snapshot = if links.is_empty() {
                    LinkSnapshot::Failed
                } else {
                    LinkSnapshot::Present(links)
                };
                (frn, snapshot)
            })
            .collect();
        Self {
            kind: MetadataSourceKind::Map { stats, links },
        }
    }

    /// Open a live NTFS volume source for per-FRN metadata reads.
    ///
    /// # Errors
    ///
    /// Returns [`super::UsnError::OpenVolume`] when the volume cannot be opened.
    #[cfg(windows)]
    pub fn open_volume(drive: &str) -> Result<Self, super::UsnError> {
        Self::open_volume_cancellable(
            drive,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    }

    #[cfg(windows)]
    pub(crate) fn open_volume_cancellable(
        drive: &str,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self, super::UsnError> {
        Ok(Self {
            kind: MetadataSourceKind::Volume(super::session::VolumeMetadataFetcher::open(
                drive, stop,
            )?),
        })
    }

    fn stat(&self, frn: u64) -> Option<(u64, i64)> {
        match &self.kind {
            MetadataSourceKind::None => None,
            MetadataSourceKind::Constant(value) => Some(*value),
            MetadataSourceKind::Map { stats, .. } => stats.get(&frn).copied(),
            #[cfg(windows)]
            MetadataSourceKind::Volume(source) => source.stat(frn),
        }
    }

    pub(crate) fn links(&self, frn: u64) -> LinkSnapshot {
        match &self.kind {
            MetadataSourceKind::None | MetadataSourceKind::Constant(_) => LinkSnapshot::Failed,
            MetadataSourceKind::Map { links, .. } => {
                links.get(&frn).cloned().unwrap_or(LinkSnapshot::Failed)
            }
            #[cfg(windows)]
            MetadataSourceKind::Volume(source) => source.links(frn),
        }
    }
}

/// Outcome tally for one applied journal batch, one counter per op kind.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BatchStats {
    /// Files/dirs created or renamed (upserted or moved in place).
    pub created_or_renamed: u32,
    /// Entries removed from the index because they were tombstoned.
    pub deleted: u32,
    /// Existing entries whose size/mtime were refreshed.
    pub stat_updated: u32,
    /// Records that resolved to no index change (e.g. delete of an entry that
    /// was never present, or a stat with no fetchable value).
    pub ignored: u32,
    /// Volume lookups (size/mtime) that came back empty — usually the file
    /// vanished before we could stat it; floods indicate a real problem.
    pub stat_failures: u32,
    /// Complete hard-link snapshots that could not be read or were empty.
    /// The entire batch is left unapplied and the worker performs a full
    /// rescan; checkpointing past one of these would make stale links
    /// permanent.
    pub hard_link_refresh_failures: u32,
    /// The batch could not be applied atomically and must not be checkpointed.
    pub rescan_required: bool,
}

struct Agg {
    reasons: u32,
    /// Index into the batch of the latest record for this FRN (carries the
    /// final name/parent/attributes).
    last: usize,
    /// Earliest old-name event in a collapsed rename storm. Its identity is
    /// the link that existed before this batch.
    rename_old: Option<usize>,
}

const STAT_REASONS: u32 = reason::DATA_OVERWRITE
    | reason::DATA_EXTEND
    | reason::DATA_TRUNCATION
    | reason::BASIC_INFO_CHANGE
    | reason::REPARSE_POINT_CHANGE;

/// Apply one journal batch. Bumps the content generation exactly once.
pub fn apply_batch(
    idx: &mut VolumeIndex,
    records: &[UsnRecord],
    fetch: &MetadataSource,
) -> BatchStats {
    let mut stats = BatchStats::default();
    let mut order: Vec<Frn> = Vec::new();
    let mut agg: FxHashMap<Frn, Agg> = FxHashMap::default();

    for (i, r) in records.iter().enumerate() {
        let key = Frn(r.frn);
        match agg.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let a = e.get_mut();
                a.reasons |= r.reason;
                a.last = i;
                if a.rename_old.is_none() && r.reason & reason::RENAME_OLD_NAME != 0 {
                    a.rename_old = Some(i);
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Agg {
                    reasons: r.reason,
                    last: i,
                    rename_old: (r.reason & reason::RENAME_OLD_NAME != 0).then_some(i),
                });
                order.push(key);
            }
        }
    }

    // Resolve every file event's link snapshot before mutating anything. Most
    // ordinary create/rename events can fall back to their journal identity,
    // but HARD_LINK_CHANGE and an ambiguous rename of a multi-link object need
    // an authoritative complete set. Preflighting those cases prevents a
    // failed lookup from committing a valid prefix of the batch.
    let mut link_snapshots = FxHashMap::default();
    for key in &order {
        let a = &agg[key];
        let link_affecting = a.reasons
            & (reason::FILE_CREATE | reason::RENAME_NEW_NAME | reason::HARD_LINK_CHANGE)
            != 0;
        if !link_affecting {
            continue;
        }
        let last = &records[a.last];
        if last.is_dir() {
            if a.reasons & reason::HARD_LINK_CHANGE != 0 {
                stats.hard_link_refresh_failures += 1;
                stats.ignored += 1;
            }
            continue;
        }
        let snapshot = fetch.links(last.frn);
        let complete = matches!(&snapshot, LinkSnapshot::Present(links) if !links.is_empty())
            || matches!(&snapshot, LinkSnapshot::Gone);
        let exact_rename_old = a.rename_old.is_some_and(|old| {
            let old = &records[old];
            idx.entry_by_link(*key, Frn(old.parent_frn), &old.name)
                .is_some()
        });
        let ambiguous_rename = a.reasons & reason::RENAME_NEW_NAME != 0
            && idx.entries_by_frn(*key).count() > 1
            && !exact_rename_old;
        if !complete && (a.reasons & reason::HARD_LINK_CHANGE != 0 || ambiguous_rename) {
            stats.hard_link_refresh_failures += 1;
            stats.ignored += 1;
        }
        link_snapshots.insert(*key, snapshot);
    }
    if stats.hard_link_refresh_failures > 0 {
        stats.rescan_required = true;
        return stats;
    }

    let first_new = idx.len() as u32;
    for key in order {
        let a = &agg[&key];
        let last = &records[a.last];

        let link_affecting = a.reasons
            & (reason::FILE_CREATE | reason::RENAME_NEW_NAME | reason::HARD_LINK_CHANGE)
            != 0;
        if link_affecting && !last.is_dir() {
            let snapshot = link_snapshots
                .remove(&key)
                .unwrap_or_else(|| fetch.links(last.frn));
            match snapshot {
                LinkSnapshot::Present(links) if !links.is_empty() => {
                    let fetched = fetch.stat(last.frn);
                    if fetched.is_none() {
                        stats.stat_failures += 1;
                    }
                    let carried = idx
                        .entry_by_frn(key)
                        .map(|id| (idx.size(id), idx.mtime(id)));
                    let (size, mtime) = fetched.or(carried).unwrap_or((0, 0));
                    let entries: Vec<_> = links
                        .iter()
                        .map(|link| RawEntry {
                            parent_frn: Frn(link.parent_frn),
                            frn: key,
                            name_utf16: &link.name,
                            is_dir: false,
                            is_reparse: last.is_reparse(),
                            is_hidden: last.is_hidden(),
                            is_system: last.is_system(),
                            size,
                            mtime,
                        })
                        .collect();
                    let LinkReconcileStats {
                        added,
                        removed,
                        retained,
                        metadata_changed,
                    } = idx.reconcile_file_links_usn(key, &entries);
                    stats.created_or_renamed += added;
                    stats.deleted += removed;
                    if fetched.is_some() && retained > 0 {
                        stats.stat_updated += 1;
                    } else if fetched.is_none() && added == 0 && removed == 0 && !metadata_changed {
                        stats.ignored += 1;
                    }
                    continue;
                }
                LinkSnapshot::Gone => {
                    let removed = idx.entries_by_frn(key).count() as u32;
                    idx.delete_frn(key);
                    if removed == 0 {
                        stats.ignored += 1;
                    } else {
                        stats.deleted += removed;
                    }
                    continue;
                }
                // HARD_LINK_CHANGE snapshots were preflighted above. Keep this
                // arm defensive for future callers that extend the preflight.
                LinkSnapshot::Failed | LinkSnapshot::Present(_)
                    if a.reasons & reason::HARD_LINK_CHANGE != 0 =>
                {
                    stats.hard_link_refresh_failures += 1;
                    stats.ignored += 1;
                    stats.rescan_required = true;
                    continue;
                }
                LinkSnapshot::Failed | LinkSnapshot::Present(_) => {
                    // Deterministic fixtures and a transient live-source miss
                    // can still apply an ordinary create/rename from the event
                    // identity below.
                }
            }
        }

        if a.reasons & reason::FILE_DELETE != 0 {
            let removed = idx.entries_by_frn(key).count() as u32;
            idx.delete_frn(key);
            if removed == 0 {
                stats.ignored += 1;
            } else {
                stats.deleted += removed;
            }
            continue;
        }

        if a.reasons & (reason::FILE_CREATE | reason::RENAME_NEW_NAME) != 0 {
            // Directory rename/move must keep the EntryId stable (children
            // point at it) — handled in place. Files go tombstone+new.
            let existing = idx.entry_by_frn(key);
            if let Some(old) = existing
                && idx.is_dir(old)
                && last.is_dir()
            {
                idx.rename_dir_frn_in_place(key, &last.name, Frn(last.parent_frn));
                idx.update_object_attrs_frn(
                    key,
                    last.is_reparse(),
                    last.is_hidden(),
                    last.is_system(),
                );
                stats.created_or_renamed += 1;
                continue;
            }
            // A rename for a no-longer-live sequence must not replace the new
            // generation now occupying the same record. A genuine FILE_CREATE
            // is precisely the event that is allowed to replace it.
            if existing.is_none()
                && a.reasons & reason::FILE_CREATE == 0
                && idx.entry_by_record(key.record()).is_some()
            {
                stats.ignored += 1;
                continue;
            }
            // Carry size/mtime over from the previous entry when the volume
            // can't answer (file already gone, or replay without fixtures).
            let fetched = fetch.stat(last.frn);
            if fetched.is_none() {
                stats.stat_failures += 1;
            }
            let carried = existing.map(|id| (idx.size(id), idx.mtime(id)));
            let (size, mtime) = fetched.or(carried).unwrap_or((0, 0));
            let entry = RawEntry {
                parent_frn: Frn(last.parent_frn),
                frn: Frn(last.frn),
                name_utf16: &last.name,
                is_dir: last.is_dir(),
                is_reparse: last.is_reparse(),
                is_hidden: last.is_hidden(),
                is_system: last.is_system(),
                size,
                mtime,
            };

            if !last.is_dir() && a.reasons & reason::RENAME_NEW_NAME != 0 {
                let before = idx.entries_by_frn(key).count();
                let removed = a.rename_old.and_then(|old| {
                    let old = &records[old];
                    idx.delete_link_frn(key, Frn(old.parent_frn), &old.name)
                });
                if removed.is_none() && before > 1 {
                    // Without either a complete snapshot or the exact old
                    // identity, choosing one of several hard links would
                    // corrupt another path. Leave the known-good set intact.
                    stats.hard_link_refresh_failures += 1;
                    stats.ignored += 1;
                    stats.rescan_required = true;
                    continue;
                }
                if removed.is_none() && before == 1 {
                    idx.upsert_usn(&entry);
                } else {
                    idx.upsert_link_usn(&entry);
                }
            } else if last.is_dir() {
                idx.upsert_usn(&entry);
            } else {
                idx.upsert_link_usn(&entry);
            }
            stats.created_or_renamed += 1;
        } else if a.reasons & STAT_REASONS != 0 {
            if idx.entry_by_frn(key).is_none() {
                // Metadata from an old record generation is stale, not a
                // fetch failure, and must not touch the current generation.
                stats.ignored += 1;
                continue;
            }
            // BASIC_INFO_CHANGE may have flipped hidden/system attributes.
            let attrs_changed = idx
                .update_object_attrs_frn(key, last.is_reparse(), last.is_hidden(), last.is_system())
                .unwrap_or(false);
            if let Some((size, mtime)) = fetch.stat(last.frn) {
                if idx.update_stat_frn(key, size, mtime).is_some() {
                    stats.stat_updated += 1;
                } else {
                    stats.ignored += 1;
                }
            } else {
                stats.stat_failures += 1;
                if !attrs_changed {
                    stats.ignored += 1;
                }
            }
        } else {
            stats.ignored += 1;
        }
    }

    idx.merge_new_into_permutations(first_new);
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::VolumeIndexBuilder;
    use crate::index::testutil::{build_hardlink_sample, u16s};
    use crate::query::{CaseMode, QueryOptions, UtcResolver, compile, parse, search};

    fn frn(sequence: u16, record: u64) -> u64 {
        (u64::from(sequence) << 48) | record
    }

    fn full_ref(record: u64) -> u64 {
        if record == 5 {
            record // root fixture keeps sequence zero
        } else {
            frn(1, record)
        }
    }

    fn rec(frn: u64, parent: u64, reason: u32, attrs: u32, name: &str) -> UsnRecord {
        rec_full(full_ref(frn), full_ref(parent), reason, attrs, name)
    }

    fn rec_full(frn: u64, parent_frn: u64, reason: u32, attrs: u32, name: &str) -> UsnRecord {
        UsnRecord {
            usn: 0,
            frn,
            parent_frn,
            reason,
            attributes: attrs,
            name: name.encode_utf16().collect(),
        }
    }

    // Real, second-aligned FILETIMEs so the u32-seconds mtime column
    // round-trips them exactly (ADR-0031): FT0 ≈ 2021-01-01, FT1 ≈ 2022-01-01.
    const FT0: i64 = 132_539_040_000_000_000;
    const FT1: i64 = 132_854_688_000_000_000;

    fn base_index() -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let docs: Vec<u16> = "docs".encode_utf16().collect();
        let note: Vec<u16> = "note.txt".encode_utf16().collect();
        b.push(RawEntry {
            parent_frn: Frn(5),
            frn: Frn(frn(1, 10)),
            name_utf16: &docs,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        b.push(RawEntry {
            parent_frn: Frn(10),
            frn: Frn(frn(1, 11)),
            name_utf16: &note,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 100,
            mtime: FT0,
        });
        b.finish()
    }

    fn path_of(idx: &VolumeIndex, record: u64) -> String {
        let id = idx.entry_by_record(record).unwrap();
        let mut p = Vec::new();
        idx.append_path(id, &mut p).unwrap();
        String::from_utf8(p).unwrap()
    }

    fn visible_names(idx: &VolumeIndex, text: &str) -> Vec<String> {
        let ast = parse(text).unwrap();
        let query = compile(&ast, CaseMode::Smart, &UtcResolver).unwrap();
        search(idx, &query, &QueryOptions::default())
            .0
            .ids
            .iter()
            .map(|&id| String::from_utf8(idx.name(id).to_vec()).unwrap())
            .collect()
    }

    fn source_with_snapshot(frn: u64, snapshot: LinkSnapshot) -> MetadataSource {
        MetadataSource {
            kind: MetadataSourceKind::Map {
                stats: HashMap::new(),
                links: HashMap::from([(frn, snapshot)]),
            },
        }
    }

    #[test]
    fn hard_link_change_reconciles_the_complete_authoritative_set() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let parent_a = frn(1, 10);
        let parent_b = frn(1, 20);
        let source = MetadataSource::map_with_links(
            HashMap::from([(object, (500, FT1))]),
            HashMap::from([(
                object,
                vec![
                    LinkInfo {
                        parent_frn: parent_a,
                        name: u16s("shared.txt"),
                    },
                    LinkInfo {
                        parent_frn: parent_b,
                        name: u16s("third.txt"),
                    },
                ],
            )]),
        );

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                object,
                parent_a,
                reason::HARD_LINK_CHANGE | reason::CLOSE,
                0x20,
                "shared.txt",
            )],
            &source,
        );

        assert_eq!(
            (stats.created_or_renamed, stats.deleted),
            (1, 1),
            "one link was added and one disappeared"
        );
        assert_eq!(stats.hard_link_refresh_failures, 0);
        assert_eq!(stats.stat_updated, 1, "the retained link was refreshed");
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_a), &u16s("shared.txt"))
                .is_some()
        );
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_b), &u16s("alias.txt"))
                .is_none()
        );
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_b), &u16s("third.txt"))
                .is_some()
        );
        for id in idx.entries_by_frn(Frn(object)) {
            assert_eq!((idx.size(id), idx.mtime(id)), (500, FT1));
        }
    }

    #[test]
    fn hard_link_refresh_failure_preserves_the_last_known_good_paths() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let before: Vec<_> = idx
            .entries_by_frn(Frn(object))
            .map(|id| (idx.parent(id), idx.name(id).to_vec()))
            .collect();

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                object,
                frn(1, 10),
                reason::FILE_DELETE | reason::HARD_LINK_CHANGE | reason::CLOSE,
                0x20,
                "shared.txt",
            )],
            &MetadataSource::none(),
        );
        let after: Vec<_> = idx
            .entries_by_frn(Frn(object))
            .map(|id| (idx.parent(id), idx.name(id).to_vec()))
            .collect();

        assert_eq!(after, before);
        assert_eq!((stats.hard_link_refresh_failures, stats.ignored), (1, 1));
        assert!(stats.rescan_required);
    }

    #[test]
    fn combined_delete_and_hard_link_change_uses_the_authoritative_live_set() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let parent_a = frn(1, 10);
        let parent_b = frn(1, 20);
        let source = MetadataSource::map_with_links(
            HashMap::new(),
            HashMap::from([(
                object,
                vec![LinkInfo {
                    parent_frn: parent_b,
                    name: u16s("alias.txt"),
                }],
            )]),
        );

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                object,
                parent_a,
                reason::FILE_DELETE | reason::HARD_LINK_CHANGE | reason::CLOSE,
                0x20,
                "shared.txt",
            )],
            &source,
        );

        assert_eq!(stats.deleted, 1);
        assert_eq!(idx.entries_by_frn(Frn(object)).count(), 1);
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_b), &u16s("alias.txt"))
                .is_some(),
            "an accumulated FILE_DELETE flag cannot erase a surviving sibling link"
        );
    }

    #[test]
    fn combined_delete_and_hard_link_change_deletes_all_only_when_gone_is_proven() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let source = source_with_snapshot(object, LinkSnapshot::Gone);

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                object,
                frn(1, 10),
                reason::FILE_DELETE | reason::HARD_LINK_CHANGE | reason::CLOSE,
                0x20,
                "shared.txt",
            )],
            &source,
        );

        assert_eq!(stats.deleted, 2);
        assert_eq!(stats.hard_link_refresh_failures, 0);
        assert_eq!(idx.entries_by_frn(Frn(object)).count(), 0);
    }

    #[test]
    fn unchanged_link_set_without_a_stat_answer_is_ignored_not_refreshed() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let source = MetadataSource::map_with_links(
            HashMap::new(),
            HashMap::from([(
                object,
                vec![
                    LinkInfo {
                        parent_frn: frn(1, 10),
                        name: u16s("shared.txt"),
                    },
                    LinkInfo {
                        parent_frn: frn(1, 20),
                        name: u16s("alias.txt"),
                    },
                ],
            )]),
        );

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                object,
                frn(1, 10),
                reason::HARD_LINK_CHANGE | reason::CLOSE,
                0x20,
                "shared.txt",
            )],
            &source,
        );

        assert_eq!(stats.stat_failures, 1);
        assert_eq!(stats.stat_updated, 0);
        assert_eq!(stats.ignored, 1);
    }

    #[test]
    fn reparse_change_without_a_stat_answer_is_applied_not_ignored() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                object,
                frn(1, 10),
                reason::REPARSE_POINT_CHANGE | reason::CLOSE,
                super::super::records::FILE_ATTRIBUTE_REPARSE_POINT,
                "shared.txt",
            )],
            &MetadataSource::none(),
        );

        assert_eq!(stats.stat_failures, 1);
        assert_eq!(stats.stat_updated, 0);
        assert_eq!(stats.ignored, 0);
        assert!(
            idx.entries_by_frn(Frn(object)).all(|id| idx.is_reparse(id)),
            "object-owned reparse state reaches every hard-link row"
        );
    }

    #[test]
    fn rename_pair_replaces_only_the_named_link_when_siblings_exist() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let parent_a = frn(1, 10);
        let parent_b = frn(1, 20);
        let batch = [
            rec_full(
                object,
                parent_a,
                reason::RENAME_OLD_NAME,
                0x20,
                "shared.txt",
            ),
            rec_full(
                object,
                parent_a,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x20,
                "renamed.txt",
            ),
        ];

        let stats = apply_batch(&mut idx, &batch, &MetadataSource::constant(42, FT1));

        assert_eq!(stats.created_or_renamed, 1);
        assert_eq!(idx.entries_by_frn(Frn(object)).count(), 2);
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_a), &u16s("shared.txt"))
                .is_none()
        );
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_a), &u16s("renamed.txt"))
                .is_some()
        );
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_b), &u16s("alias.txt"))
                .is_some(),
            "the other hard link survives"
        );
    }

    #[test]
    fn ambiguous_multi_link_rename_rejects_the_entire_batch_before_mutation() {
        let mut idx = build_hardlink_sample();
        let object = frn(1, 100);
        let parent_a = frn(1, 10);
        let generation = idx.content_generation();
        let batch = [
            rec_full(
                frn(1, 200),
                parent_a,
                reason::FILE_CREATE | reason::CLOSE,
                0x20,
                "prefix.txt",
            ),
            rec_full(
                object,
                parent_a,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x20,
                "ambiguous.txt",
            ),
        ];

        let stats = apply_batch(&mut idx, &batch, &MetadataSource::none());

        assert!(stats.rescan_required);
        assert_eq!(stats.hard_link_refresh_failures, 1);
        assert_eq!(stats.created_or_renamed, 0);
        assert_eq!(idx.content_generation(), generation);
        assert!(idx.entry_by_frn(Frn(frn(1, 200))).is_none());
        assert_eq!(idx.entries_by_frn(Frn(object)).count(), 2);
        assert!(
            idx.entry_by_link(Frn(object), Frn(parent_a), &u16s("shared.txt"))
                .is_some(),
            "the known-good link set is unchanged"
        );
    }

    #[test]
    fn create_in_new_dir_within_one_batch() {
        let mut idx = base_index();
        let batch = [
            rec(20, 5, reason::FILE_CREATE | reason::CLOSE, 0x10, "src"),
            rec(21, 20, reason::FILE_CREATE | reason::CLOSE, 0x20, "main.rs"),
        ];
        let s = apply_batch(&mut idx, &batch, &MetadataSource::constant(42, FT0));
        assert_eq!(s.created_or_renamed, 2);
        assert_eq!(path_of(&idx, 21), r"C:\src\main.rs");
        let id = idx.entry_by_record(21).unwrap();
        assert_eq!((idx.size(id), idx.mtime(id)), (42, FT0));
    }

    #[test]
    fn rename_storm_collapses_to_final_name() {
        let mut idx = base_index();
        let batch = [
            rec(11, 10, reason::RENAME_OLD_NAME, 0x20, "note.txt"),
            rec(11, 10, reason::RENAME_NEW_NAME, 0x20, "tmp1.txt"),
            rec(11, 10, reason::RENAME_OLD_NAME, 0x20, "tmp1.txt"),
            rec(
                11,
                10,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x20,
                "final.txt",
            ),
        ];
        let s = apply_batch(&mut idx, &batch, &MetadataSource::none());
        assert_eq!(s.created_or_renamed, 1);
        assert_eq!(path_of(&idx, 11), r"C:\docs\final.txt");
        // Carried over size/mtime survive a rename without a fetcher.
        let id = idx.entry_by_record(11).unwrap();
        assert_eq!((idx.size(id), idx.mtime(id)), (100, FT0));
    }

    #[test]
    fn move_to_other_dir_updates_child_paths() {
        let mut idx = base_index();
        let batch = [
            rec(20, 5, reason::FILE_CREATE | reason::CLOSE, 0x10, "archive"),
            rec(
                10,
                20,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x10,
                "docs",
            ),
        ];
        apply_batch(&mut idx, &batch, &MetadataSource::none());
        // docs moved under archive; note.txt's lazy path follows.
        assert_eq!(path_of(&idx, 11), r"C:\archive\docs\note.txt");
    }

    #[test]
    fn create_then_delete_in_one_batch_is_a_delete() {
        let mut idx = base_index();
        let n = idx.live_len();
        let batch = [
            rec(30, 5, reason::FILE_CREATE, 0x20, "ghost.tmp"),
            rec(
                30,
                5,
                reason::FILE_DELETE | reason::CLOSE,
                0x20,
                "ghost.tmp",
            ),
        ];
        let s = apply_batch(&mut idx, &batch, &MetadataSource::none());
        assert_eq!(s.deleted, 0); // never existed in the index
        assert_eq!(s.ignored, 1);
        assert_eq!(idx.live_len(), n);
    }

    #[test]
    fn stat_update_changes_size_and_mtime() {
        let mut idx = base_index();
        let batch = [rec(
            11,
            10,
            reason::DATA_EXTEND | reason::CLOSE,
            0x20,
            "note.txt",
        )];
        let s = apply_batch(&mut idx, &batch, &MetadataSource::constant(5000, FT1));
        assert_eq!(s.stat_updated, 1);
        let id = idx.entry_by_record(11).unwrap();
        assert_eq!((idx.size(id), idx.mtime(id)), (5000, FT1));
    }

    #[test]
    fn delete_removes_from_results_and_generation_bumps() {
        let mut idx = base_index();
        let g0 = idx.content_generation();
        let batch = [rec(
            11,
            10,
            reason::FILE_DELETE | reason::CLOSE,
            0x20,
            "note.txt",
        )];
        let s = apply_batch(&mut idx, &batch, &MetadataSource::none());
        assert_eq!(s.deleted, 1);
        assert!(idx.entry_by_record(11).is_none());
        assert_eq!(idx.content_generation(), g0 + 1);
    }

    #[test]
    fn renamed_entry_lands_sorted_in_permutation() {
        let mut idx = base_index();
        let batch = [rec(
            11,
            10,
            reason::RENAME_NEW_NAME | reason::CLOSE,
            0x20,
            "aaa_first.txt",
        )];
        apply_batch(&mut idx, &batch, &MetadataSource::none());
        let perm = idx.name_permutation();
        let live: Vec<&[u8]> = perm
            .iter()
            .filter(|&&id| idx.is_live(id))
            .map(|&id| idx.lower_name(id))
            .collect();
        let mut sorted = live.clone();
        sorted.sort();
        assert_eq!(live, sorted);
    }

    #[test]
    fn recycled_record_old_delete_then_new_create_keeps_new_generation() {
        let mut idx = base_index();
        let old = frn(1, 11);
        let new = frn(2, 11);
        let docs = frn(1, 10);
        let batch = [
            rec_full(
                old,
                docs,
                reason::FILE_DELETE | reason::CLOSE,
                0x20,
                "note.txt",
            ),
            rec_full(
                new,
                docs,
                reason::FILE_CREATE | reason::CLOSE,
                0x20,
                "reborn.txt",
            ),
        ];

        let stats = apply_batch(&mut idx, &batch, &MetadataSource::constant(7, FT1));

        assert_eq!((stats.deleted, stats.created_or_renamed), (1, 1));
        assert!(idx.entry_by_frn(Frn(old)).is_none());
        let id = idx.entry_by_frn(Frn(new)).expect("new generation is live");
        assert_eq!(idx.entry_by_record(11), Some(id));
        assert_eq!(idx.name(id), b"reborn.txt");
    }

    #[test]
    fn delayed_old_generation_delete_cannot_remove_recycled_record() {
        let mut idx = base_index();
        let old = frn(1, 11);
        let new = frn(2, 11);
        let docs = frn(1, 10);
        apply_batch(
            &mut idx,
            &[rec_full(
                new,
                docs,
                reason::FILE_CREATE | reason::CLOSE,
                0x20,
                "current.txt",
            )],
            &MetadataSource::constant(9, FT1),
        );
        let current = idx.entry_by_frn(Frn(new)).unwrap();

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                old,
                docs,
                reason::FILE_DELETE | reason::CLOSE,
                0x20,
                "old.txt",
            )],
            &MetadataSource::none(),
        );

        assert_eq!((stats.deleted, stats.ignored), (0, 1));
        assert_eq!(idx.entry_by_frn(Frn(new)), Some(current));
        assert!(idx.is_live(current));
        assert_eq!(idx.name(current), b"current.txt");

        let stale_stat = apply_batch(
            &mut idx,
            &[rec_full(
                old,
                docs,
                reason::DATA_EXTEND | reason::BASIC_INFO_CHANGE | reason::CLOSE,
                0x2 | 0x20,
                "old.txt",
            )],
            &MetadataSource::constant(999, FT0),
        );
        assert_eq!((stale_stat.stat_updated, stale_stat.ignored), (0, 1));
        assert_eq!((idx.size(current), idx.mtime(current)), (9, FT1));
        assert!(
            !idx.is_excluded(current),
            "stale attributes cannot hide the new generation"
        );
    }

    #[test]
    fn recycled_directory_is_replaced_not_renamed_and_stale_rename_is_ignored() {
        let mut idx = base_index();
        let old = frn(1, 10);
        let new = frn(2, 10);
        let old_id = idx.entry_by_frn(Frn(old)).unwrap();

        apply_batch(
            &mut idx,
            &[rec_full(
                new,
                5,
                reason::FILE_CREATE | reason::CLOSE,
                0x10,
                "newdocs",
            )],
            &MetadataSource::none(),
        );
        let new_id = idx.entry_by_frn(Frn(new)).unwrap();
        assert_ne!(new_id, old_id, "record reuse must not rename in place");
        assert!(!idx.is_live(old_id));

        let stats = apply_batch(
            &mut idx,
            &[rec_full(
                old,
                5,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x10,
                "stale-old-name",
            )],
            &MetadataSource::none(),
        );
        assert_eq!((stats.created_or_renamed, stats.ignored), (0, 1));
        assert_eq!(idx.entry_by_frn(Frn(new)), Some(new_id));
        assert_eq!(idx.name(new_id), b"newdocs");
    }

    fn exclusion_index() -> VolumeIndex {
        let mut b = VolumeIndexBuilder::new("C:", 5);
        let hidden: Vec<u16> = "hidden".encode_utf16().collect();
        let visible: Vec<u16> = "visible".encode_utf16().collect();
        let subtree: Vec<u16> = "subtree".encode_utf16().collect();
        let leaf: Vec<u16> = "needle.txt".encode_utf16().collect();
        b.push(RawEntry {
            parent_frn: Frn(5),
            frn: Frn(frn(1, 20)),
            name_utf16: &hidden,
            is_dir: true,
            is_reparse: false,
            is_hidden: true,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        b.push(RawEntry {
            parent_frn: Frn(5),
            frn: Frn(frn(1, 30)),
            name_utf16: &visible,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        b.push(RawEntry {
            parent_frn: Frn(30),
            frn: Frn(frn(1, 40)),
            name_utf16: &subtree,
            is_dir: true,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 0,
            mtime: 0,
        });
        b.push(RawEntry {
            parent_frn: Frn(40),
            frn: Frn(frn(1, 41)),
            name_utf16: &leaf,
            is_dir: false,
            is_reparse: false,
            is_hidden: false,
            is_system: false,
            size: 1,
            mtime: FT0,
        });
        b.finish()
    }

    #[test]
    fn directory_moves_and_renames_recompute_descendant_query_visibility() {
        let mut idx = exclusion_index();
        let subtree = frn(1, 40);
        let hidden = frn(1, 20);
        let visible = frn(1, 30);
        let leaf = idx.entry_by_frn(Frn(frn(1, 41))).unwrap();
        assert_eq!(visible_names(&idx, "needle"), vec!["needle.txt"]);

        apply_batch(
            &mut idx,
            &[rec_full(
                subtree,
                hidden,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x10,
                "inside-hidden",
            )],
            &MetadataSource::none(),
        );
        assert!(idx.is_excluded(leaf));
        assert!(visible_names(&idx, "needle").is_empty());

        apply_batch(
            &mut idx,
            &[rec_full(
                subtree,
                hidden,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x10,
                "renamed-again",
            )],
            &MetadataSource::none(),
        );
        assert!(
            idx.is_excluded(leaf),
            "name-only rename preserves inheritance"
        );

        apply_batch(
            &mut idx,
            &[rec_full(
                subtree,
                visible,
                reason::RENAME_NEW_NAME | reason::CLOSE,
                0x10,
                "outside-hidden",
            )],
            &MetadataSource::none(),
        );
        assert!(!idx.is_excluded(leaf));
        assert_eq!(visible_names(&idx, "needle"), vec!["needle.txt"]);
    }

    #[test]
    fn directory_hidden_attribute_toggle_recomputes_all_descendants() {
        let mut idx = exclusion_index();
        let subtree = frn(1, 40);
        let visible = frn(1, 30);
        let leaf = idx.entry_by_frn(Frn(frn(1, 41))).unwrap();

        apply_batch(
            &mut idx,
            &[rec_full(
                subtree,
                visible,
                reason::BASIC_INFO_CHANGE | reason::CLOSE,
                0x10 | 0x2,
                "subtree",
            )],
            &MetadataSource::none(),
        );
        assert!(idx.is_excluded(leaf));
        assert!(visible_names(&idx, "needle").is_empty());

        apply_batch(
            &mut idx,
            &[rec_full(
                subtree,
                visible,
                reason::BASIC_INFO_CHANGE | reason::CLOSE,
                0x10,
                "subtree",
            )],
            &MetadataSource::none(),
        );
        assert!(!idx.is_excluded(leaf));
        assert_eq!(visible_names(&idx, "needle"), vec!["needle.txt"]);
    }
}
