# ADR-0024: Non-elevated scope index mode (folder walk + ReadDirectoryChangesW)

Date: 2026-06-16 / Status: Adopted

## Decision

Add a **scope mode** that, without administrator privileges (asInvoker), indexes only a user-specified set of roots. The index source is not MFT/USN but **folder walk (directory enumeration) + ReadDirectoryChangesW**. It runs in-proc inside the UI process without a service, and the index is placed in `%LOCALAPPDATA%\find-my-files\` (per-user, its own `.writer.lock`).

This does not break ADR-0018's "two-seam cap, no additional ports." Scope mode fits as a **second implementation** of the existing two seams:

- Snapshot creation side (the scan, outside the seam): place `scan::walk::walk_scan` next to `mft::scan_volume` and branch at the single establish point in `worker.rs`.
- `SnapshotStore`: reuse `WinSnapshotStore` as-is (only the path becomes `%LOCALAPPDATA%`).
- `JournalSource`: add `WatcherJournalSource` (ReadDirectoryChangesW) as a second implementation alongside the USN `WinJournalSource`.

This **amends, for this one point only**, ADR-0001's "no folder-walk indexing / volume-granularity only" (see "Relationship to ADR-0001" below).

## Rationale

- The target (search-heavy business users) have company PCs where the IT department prohibits elevation. Direct MFT reads, USN, and service installation all require administrator, locking the product out entirely. The only mature non-elevated path to index real data is folder walk + ReadDirectoryChangesW.
- All of C: is given up, but limited to the roots a knowledge worker actually touches (profile / OneDrive·SharePoint sync / mapped drives / project folders = tens of thousands to hundreds of thousands of entries), the walk finishes in seconds and freshness is maintained.
- **No index-format change is needed thanks to synthetic FRN**: the index's identity key is `Frn::record()` = lower 48 bits (`index/mod.rs`), a design that resolves NTFS record reuse by liveness. Real FRNs are unnecessary. Scope mode uses `frn = xxhash64(folded absolute path)`. Absolute paths are globally unique, so the lower 48 bits are unique across roots too, with no need for a separate root id (`record()` discards the upper bits, so a root id in the upper bits would not help lookup). The watcher can recompute the same hash statelessly from the changed path.
- Multi-root is expressible with no format change: push each root as a child of ROOT (name = absolute base path), and the existing path reconstruction returns the correct absolute path as-is.
- The query path is the same `VolumeIndex`, so it is unchanged and does not affect the p99 hard line (<10ms perceived for 3+ characters).

## Consequences

- **The amendment is minimal**: the "filename-only indexing" core is preserved. content index, property/tag index, and preview remain not adopted (ADR-0001). Even in scope mode, only filename, size, modified time, and attributes are kept.
- Synthetic FRN becomes a different ID when a rename changes the path. The watcher translates ReadDirectoryChangesW old/new path pairs into `delete(hash(old)) + create(hash(new))` and reuses `apply_batch`. Directory rename triggers a subtree re-walk of the new path (rare, bounded, same accepted-limitation class as dir-rename).
- Lower-48-bit collisions are probabilistic (<0.1% at hundreds of thousands of entries). A collision merely shadows one file by another and self-heals on re-walk (manual re-index / periodic re-walk / journal-gone equivalent).
- Freshness: because ReadDirectoryChangesW can drop events on network/cloud (OneDrive placeholder), a **periodic re-walk** is the safety net. Placeholders are indexed with enumeration metadata only and are never hydrated (avoiding data charges and performance incidents).
- It is not mutually exclusive with the existing elevated modes (service/inproc, all volumes); it is the option when no service is present and not elevated. If no configuration exists, the setup screen guides as before.

## Relationship to ADR-0001

This ADR overrides the consequences section of ADR-0001, "volume-granularity only / no folder-walk indexing." The heart that ADR-0001 protects (filename-only indexing, content-index exclusion, scope-creep prevention) is unchanged. What this ADR permits is only the one point of "indexing non-elevated-readable roots via folder walk"; FTP/HTTP/ETP servers, FAT/exFAT, ReFS, and cross-platform support remain not adopted.

## Re-examination triggers

- If scope-mode cold-start (a walk of hundreds of thousands of entries) is slow to a degree that hurts the perceived experience → re-evaluate rayon parallel root walk and `NtQueryDirectoryFile` bulking (Phase 3).
- If lower-48-bit collisions are observed in a real index at a non-negligible frequency (monitored via a measurement counter) → redesign the record numbering scheme.
- If ReadDirectoryChangesW drops cannot be fully absorbed by periodic re-walk → refine per-root overflow recovery.
