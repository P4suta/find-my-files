# ADR-0015: WinUI 3 data virtualization (non-generic IList+INCC+IItemsRangeInfo)

Date: 2026-06-11 / Status: Accepted

## Decision

Result-list virtualization uses non-generic `IList` + `INotifyCollectionChanged` + `IItemsRangeInfo` + placeholders (VirtualResultList). Do not use `ISupportIncrementalLoading`, ItemsView, or ItemsRepeater. ItemsPanel is fixed to ItemsStackPanel. VirtualResultList is a single instance with the same lifetime as the page (x:Bind OneTime), and ItemsSource is not swapped. New results are published via `Reassign` (apply prefetched seed + one INCC Reset); a re-query where the engine returns `QueryTrace.unchanged=true` (same query, ID sequence memcmp-equal across the whole volume) uses `RefreshInPlace` (no Reset, in-place fill of visible rows, count text unchanged).

## Rationale

- For random-access virtualization with a known count, "non-generic IList + INCC + IItemsRangeInfo + placeholders" is the explicitly supported path in current WASDK. `IList<T>` alone does not work (microsoft-ui-xaml#1809).
- `ISupportIncrementalLoading` has crash reports, so avoid it (microsoft-ui-xaml#6883).
- **The three interfaces are one indivisible requirement, not a style choice.** Each is load-bearing and each substitution has a known failure: dropping the non-generic `IList` (keeping only `IList<T>`) is the #1809 configuration and does not virtualize; reaching for `ISupportIncrementalLoading` instead of `IItemsRangeInfo` is the #6883 crash; dropping `INotifyCollectionChanged` leaves no way to publish a new result set without swapping ItemsSource. There is no partial adoption that degrades gracefully — the fallbacks crash or silently de-virtualize, which is why this set is fixed here rather than left to the call site.
- ItemsView / ItemsRepeater do not support the above interfaces. Setting ItemsPanel to anything other than ItemsStackPanel disables virtualization.
- Swapping ItemsSource discards the ListView's virtualization state and reintroduces flicker.
- Windows is never silent even when idle (USN batches from logs, telemetry, etc.). IndexChanged-driven re-queries return identical results every 200ms, so re-issuing Reset would churn the screen constantly — RefreshInPlace on unchanged (the MVVM setter notifies only on value change) brings redraw of the same screen to zero.

## Consequences

- **IList residency contract (never falsely affirm membership).** XAML consumes `Contains` / `IndexOf` / `GetAt` through the WinRT adapter and blindly trusts the answers, and the two ways of being wrong are **not symmetric**: a false "absent" is self-correcting (the container is simply re-realized), while a false "present" crashes deep inside XAML at `GetAt(staleIndex)`, in frames this code cannot catch. Residency therefore fails closed and is defined narrowly = "index is less than Count, **and** the corresponding slot in the current page cache is that same instance". A row belonging to an older result epoch, a row whose page the LRU has evicted, and a temporary row materialized for enumeration all answer absent. (Demonstrated: search with results -> clear all reliably reproduces an `Int32.MaxValue-1` exception. Fix A/B: UIA stress went from 4 errors on the old code to 0.)
- The indexer throws immediately out of range and never fetches (returns a placeholder). Enumeration/CopyTo do not disturb the page LRU (cap 4096 rows).
- The UI-thread check in Reassign/RefreshInPlace is always enabled in Release.
- In-place updates only update cells whose value changed (e.g. the size of a grown file).

## Re-examination triggers

- If WASDK officially provides known-count random-access virtualization (IItemsRangeInfo equivalent) for the ItemsView family.
