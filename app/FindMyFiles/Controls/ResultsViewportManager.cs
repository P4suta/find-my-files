using Microsoft.UI.Xaml.Controls;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;

namespace FindMyFiles.Controls;

/// <summary>
/// Imperative ListView companion for the results list: viewport placement
/// after each published result, best-effort selection restore across seeded
/// Resets, ScrollViewer discovery, and the selection-driven row actions
/// (open / reveal / copy — all via <see cref="ShellOps"/>, which sheds
/// elevation through explorer.exe).
///
/// UI thread only: every member touches the ListView, and the class is
/// constructed and wired from the page constructor on the UI thread. The
/// pure decision helpers (<see cref="CanRestoreViewport"/>,
/// <see cref="FindSelectionIndex"/>, <see cref="CopyablePaths"/>) are static
/// and unit-tested without a ListView.
/// </summary>
public sealed class ResultsViewportManager
{
    private readonly ListView _list;
    private ScrollViewer? _scroller;
    private ulong? _lastSelectedEntryRef;

    public ResultsViewportManager(ListView list)
    {
        _list = list;
        _list.SelectionChanged += (_, _) =>
        {
            // Remember the last real selection so a position-preserving
            // requery can re-find it (Reset clears the ListView selection).
            if (_list.SelectedItem is ResultRow { IsPlaceholder: false } row)
            {
                _lastSelectedEntryRef = row.EntryRef;
            }
        };
    }

    // ── Viewport placement after each published result ──────────────────

    /// <summary>
    /// Reset origins (typing, sort…) land at the top; position-preserving
    /// origins (index changed, stale…) restore the previous first visible row
    /// and, best effort, the selection. Explicit placement — the ListView's
    /// own behavior after a Reset is version-dependent.
    /// </summary>
    public void OnResultsPublished(ResultsPublication pub)
    {
        if (CanRestoreViewport(pub.RestoreIndex, _list.Items.Count))
        {
            var restore = pub.RestoreIndex!.Value;
            _list.ScrollIntoView(_list.Items[restore], ScrollIntoViewAlignment.Leading);
            RestoreSelection(pub);
        }
        else
        {
            _scroller ??= FindScrollViewer(_list);
            _scroller?.ChangeView(null, 0, null, disableAnimation: true);
        }
    }

    private void RestoreSelection(ResultsPublication pub)
    {
        if (_lastSelectedEntryRef is not { } entryRef)
        {
            return;
        }
        if (FindSelectionIndex(
                i => _list.Items[i], _list.Items.Count,
                pub.FirstSeededIndex, pub.LastSeededIndex, entryRef) is { } index)
        {
            _list.SelectedIndex = index;
        }
    }

    // ── Selection-driven row actions (keyboard, double-click, menu) ─────

    public ResultRow? SelectedRow() => _list.SelectedItem as ResultRow;

    public ResultRow? SelectedOrTopRow() =>
        SelectedRow() ?? (_list.Items.Count > 0 ? _list.Items[0] as ResultRow : null);

    public void OpenSelected() => Open(SelectedRow());

    public void OpenSelectedOrTop() => Open(SelectedOrTopRow());

    public void RevealSelected()
    {
        if (SelectedRow() is { IsPlaceholder: false } row)
        {
            ShellOps.Reveal(row.FullPath);
        }
    }

    public void CopySelectedPaths()
    {
        var paths = CopyablePaths(_list.SelectedItems);
        if (paths.Count > 0)
        {
            ShellOps.CopyText(string.Join("\r\n", paths), "paths");
        }
    }

    /// <summary>Down from the search box: focus the list on its top row.</summary>
    public void FocusTopRow()
    {
        _list.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
        if (_list.Items.Count > 0)
        {
            _list.SelectedIndex = 0;
            _list.ScrollIntoView(_list.SelectedItem);
        }
    }

    private static void Open(ResultRow? row)
    {
        if (row is { IsPlaceholder: false })
        {
            ShellOps.Open(row.FullPath);
        }
    }

    // ── Pure decision helpers (unit-tested without a ListView) ──────────

    /// <summary>A restore index is honored only when it addresses a row that
    /// exists in the newly published result.</summary>
    internal static bool CanRestoreViewport(int? restoreIndex, int itemCount) =>
        restoreIndex is { } restore && restore >= 0 && restore < itemCount;

    /// <summary>
    /// Re-find the previously selected row (by engine identity) inside the
    /// seeded window of the new result; null when it is gone — the selection
    /// then deliberately stays cleared rather than guessed.
    /// </summary>
    internal static int? FindSelectionIndex(
        Func<int, object?> itemAt,
        int itemCount,
        int firstSeededIndex,
        int lastSeededIndex,
        ulong entryRef)
    {
        for (var i = firstSeededIndex; i <= lastSeededIndex && i < itemCount; i++)
        {
            if (itemAt(i) is ResultRow { IsPlaceholder: false } row && row.EntryRef == entryRef)
            {
                return i;
            }
        }
        return null;
    }

    /// <summary>Real (non-placeholder) full paths of a selection, in order.</summary>
    internal static List<string> CopyablePaths(System.Collections.IEnumerable selectedItems) =>
        [.. selectedItems
            .OfType<ResultRow>()
            .Where(r => !r.IsPlaceholder)
            .Select(r => r.FullPath)];

    private static ScrollViewer? FindScrollViewer(Microsoft.UI.Xaml.DependencyObject root)
    {
        for (var i = 0; i < Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(root, i);
            if (child is ScrollViewer viewer)
            {
                return viewer;
            }
            if (FindScrollViewer(child) is { } nested)
            {
                return nested;
            }
        }
        return null;
    }
}
