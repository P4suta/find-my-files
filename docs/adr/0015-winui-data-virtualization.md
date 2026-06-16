# ADR-0015: WinUI 3 data virtualization (non-generic IList+INCC+IItemsRangeInfo)

Date: 2026-06-11 / Status: Accepted

## Decision

Result-list virtualization uses non-generic `IList` + `INotifyCollectionChanged` + `IItemsRangeInfo` + placeholders (VirtualResultList). Do not use `ISupportIncrementalLoading`, ItemsView, or ItemsRepeater. ItemsPanel is fixed to ItemsStackPanel. VirtualResultList is a single instance with the same lifetime as the page (x:Bind OneTime), and ItemsSource is not swapped. New results are published via `Reassign` (apply prefetched seed + one INCC Reset); a re-query where the engine returns `QueryTrace.unchanged=true` (same query, ID sequence memcmp-equal across the whole volume) uses `RefreshInPlace` (no Reset, in-place fill of visible rows, count text unchanged).

## Rationale

- For random-access virtualization with a known count, "non-generic IList + INCC + IItemsRangeInfo + placeholders" is the explicitly supported path in current WASDK. `IList<T>` alone does not work (microsoft-ui-xaml#1809).
- `ISupportIncrementalLoading` has crash reports, so avoid it (microsoft-ui-xaml#6883).
- ItemsView / ItemsRepeater do not support the above interfaces. Setting ItemsPanel to anything other than ItemsStackPanel disables virtualization.
- Swapping ItemsSource discards the ListView's virtualization state and reintroduces flicker.
- Windows is never silent even when idle (USN batches from logs, telemetry, etc.). IndexChanged-driven re-queries return identical results every 200ms, so re-issuing Reset would churn the screen constantly — RefreshInPlace on unchanged (the MVVM setter notifies only on value change) brings redraw of the same screen to zero.

## Consequences

- IList residency contract: residency = "index is less than Count, and the corresponding slot in the current page cache is that same instance". A false "residency" causes `GetAt(staleIndex)` to crash deep in XAML (demonstrated: search with results -> clear all reliably reproduces an `Int32.MaxValue-1` exception. Fix A/B: UIA stress went from 4 errors on the old code to 0).
- The indexer throws immediately out of range and never fetches (returns a placeholder). Enumeration/CopyTo do not disturb the page LRU (cap 4096 rows).
- The UI-thread check in Reassign/RefreshInPlace is always enabled in Release.
- In-place updates only update cells whose value changed (e.g. the size of a grown file).

## Re-examination triggers

- If WASDK officially provides known-count random-access virtualization (IItemsRangeInfo equivalent) for the ItemsView family.
