# ADR-0025: Scope-mode excludes prune at walk time (not query time)

Date: 2026-06-17 / Status: Adopted

## Decision

Let scope mode (ADR-0024) take a set of **exclude** paths alongside its roots, and honour them by **pruning at folder-walk time**: when the walk reaches a directory whose folded absolute path matches an exclude, it skips that entry and does not descend, so the excluded subtree is never indexed. Excludes flow `AppSettings.ScopeExcludes` → `fmf_index_start_scope(roots, n, excludes, m)` → `Engine::index_start_scope(roots, excludes)` → `WorkerKind::Walk { roots, excludes }` → `walk_scan(roots, excludes)`.

The UI restricts an exclude to a subfolder under one of the selected roots (`ScopePaths.IsUnderAnyRoot`), and collapses nested roots before persisting (`ScopePaths.Normalize`); the engine just prunes whatever folded paths it is handed.

## Rationale

- **Scope mode exists to keep the index small** (ADR-0024: "only the roots a knowledge worker actually touches"). The common excludes — `node_modules`, build output, `.git`, archive trees — are exactly the high-file-count directories a developer does *not* want searched. Pruning them at walk time keeps them out of RAM and shortens the cold walk; that is the first-principles fit for scope mode, not a bolt-on filter.
- **The alternative, query-time exclusion** (reuse the ADR-0019 `!path:` rewrite, appending the excludes to every query) was rejected: it still indexes every excluded entry (RAM and walk-time wasted, contradicting the lean-index premise) and adds a per-query path-evaluation cost on the hot search path against the single-digit-ms bar. Index-time pruning costs one hash lookup per directory and zero per query.
- **Matching is allocation-free.** The walk already folds each entry's path (ADR-0003, per-char length-preserving) to build its synthetic-FRN record key. Excludes are folded the same way once, up front, into a `HashSet<Vec<u8>>`; the prune check is a single case-insensitive lookup of the path already in hand. Exact-match (not prefix) on the directory's full folded path, so `archive` never prunes a sibling `archive2`, and pruning at the subtree root means descendants are unreachable without per-descendant checks.
- **No index-format or contract-POD change.** Excludes ride the existing FFI as a second string array; `fmf_index_start_scope`'s signature is extended in place (scope mode is FFI-only and co-shipped with the DLL — no external ABI consumer, no opcode, no `FmfVolumeStatus`/POD change), so `ABI_VERSION` is unchanged and `contract/golden` is untouched. The export-signature pin in `fmf-ffi/src/lib.rs` is updated to the new arity.

## Consequences

- Changing excludes (like changing roots) is **not a live operation**: the engine no-ops `index_start_scope` on an existing scope slot, so the UI persists and relaunches into a fresh `WalkInProc` that re-walks with the new set — the same save+relaunch model the roots re-selection already uses (`MainViewModel.ApplyScopeChange`).
- The prune count is **normal behaviour, not a degradation**: it is returned in `ScanStats::walk_excluded_pruned` and surfaced in the `full scan complete` log, deliberately *not* as a degrade counter (unlike `walk_read_errors` / `walk_depth_truncated`, which signal incompleteness). If exclude effectiveness later needs a metric, promote it to a counter via the 3-point set (metrics + `fmf-contract::counters::COUNTER_NAMES` + `just contract-gen`).
- **Folded-path canonicalization.** A root that folds to a trailing separator (drive roots: `C:\` → `c:\`) previously seeded its children's keys as `c:\\users` (doubled). The root's `Pending.folded` now strips one trailing separator, so children read `c:\users` — matching `fold(full_path)`, the exclude set, and a future watcher's stateless recompute. This corrects a latent inconsistency for drive-root scopes; ordinary roots (no trailing separator) are unaffected.
- **The Phase 2 watcher must also honour excludes.** `WatcherJournalSource` is still a no-op stub; when it lands (ReadDirectoryChangesW), changes under an excluded subtree must be filtered the same way, or excluded files would re-enter the index on edit. Tracked as a Phase 2 obligation.

## Relationship to ADR-0024

This refines scope mode; it does not touch the privileged ($MFT/USN) path. The "filename-only indexing" core and the two-seam cap (ADR-0018) are unchanged — pruning happens inside the scan (outside the seam), exactly where `walk_scan` already lives.

## Re-examination triggers

- If users want excludes that are **name/glob patterns** (`node_modules`, `*.tmp`) applied across all roots rather than specific subfolder paths → extend the match from exact folded-path to a name/glob test in the walk (the prune point is the same).
- If an exclude needs to apply **at query time** (e.g. a saved search that hides a subtree without re-walking) → revisit the ADR-0019 `!path:` rewrite as a complementary, not replacement, mechanism.
