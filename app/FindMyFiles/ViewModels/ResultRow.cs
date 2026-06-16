using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Highlighting;

namespace FindMyFiles.ViewModels;

/// <summary>
/// One list row. Created as a placeholder by the virtualized list and filled
/// in place when its page arrives — the same instance stays bound, so no
/// container regeneration happens (docs/ARCHITECTURE.md).
/// </summary>
public sealed partial class ResultRow : ObservableObject
{
    /// <summary>Shared empty range list: every un-highlighted row points here,
    /// so an equal-empty refill compares reference-equal and notifies nothing
    /// (keeps a same-query RefreshInPlace from repainting).</summary>
    private static readonly IReadOnlyList<HighlightRange> NoRanges = [];

    /// <summary>Absolute position of this row in the full result set — the
    /// virtualized list's stable key, set once at placeholder creation and
    /// never changed (the same instance is refilled when its page lands).</summary>
    public long Index { get; init; }

    /// <summary>File or folder name (leaf). Empty until <see cref="Fill"/>.</summary>
    [ObservableProperty]
    public partial string Name { get; set; } = string.Empty;

    /// <summary>Containing directory, trailing-separator included so
    /// <c>ParentPath + Name</c> is the full path. Empty until <see cref="Fill"/>.</summary>
    [ObservableProperty]
    public partial string ParentPath { get; set; } = string.Empty;

    /// <summary>Human-formatted size (B/KB/MB/GB); empty for directories.</summary>
    [ObservableProperty]
    public partial string SizeText { get; set; } = string.Empty;

    /// <summary>Local modified time formatted <c>yyyy/MM/dd HH:mm</c>; empty
    /// when the engine reports no mtime.</summary>
    [ObservableProperty]
    public partial string DateText { get; set; } = string.Empty;

    /// <summary>Segoe Fluent Icons glyph for the row's type (folder vs page).</summary>
    [ObservableProperty]
    public partial string Glyph { get; set; } = ""; // Page

    /// <summary>Ranges of <see cref="Name"/> the query matched, for the name
    /// TextBlock's highlight; empty when nothing matches. Filled together with
    /// <see cref="Name"/> so the text and its emphasis never disagree.</summary>
    [ObservableProperty]
    public partial IReadOnlyList<HighlightRange> NameRanges { get; set; } = NoRanges;

    /// <summary>Ranges of <see cref="ParentPath"/> a path term matched (a query
    /// term containing <c>\</c> or <c>path:</c>); empty otherwise.</summary>
    [ObservableProperty]
    public partial IReadOnlyList<HighlightRange> PathRanges { get; set; } = NoRanges;

    /// <summary>Absolute path (<c>ParentPath + Name</c>) — what open/copy act on.
    /// Empty until <see cref="Fill"/>.</summary>
    public string FullPath { get; private set; } = string.Empty;

    /// <summary>True while this is an unfilled placeholder (its page hasn't
    /// arrived); the row template shows a skeleton until <see cref="Fill"/>.</summary>
    public bool IsPlaceholder { get; private set; } = true;

    /// <summary>Engine row identity — lets the view re-find a selected row
    /// after a position-preserving requery (best effort).</summary>
    public ulong EntryRef { get; private set; }

    /// <summary>Make an empty row for <paramref name="index"/> — the
    /// virtualized list's only constructor, called for every slot before any
    /// data is fetched.</summary>
    /// <param name="index">Absolute position of the row in the full result set.</param>
    /// <returns>A new placeholder row keyed to <paramref name="index"/>.</returns>
    public static ResultRow CreatePlaceholder(long index) => new() { Index = index };

    /// <summary>Populate this placeholder from an engine <see cref="RowData"/>
    /// page hit: copies identity (<see cref="EntryRef"/>, <see cref="FullPath"/>),
    /// formats size/date for display, picks the type glyph, and clears
    /// <see cref="IsPlaceholder"/>. In place — the bound instance is reused.</summary>
    /// <param name="data">The engine page hit supplying this row's identity and fields.</param>
    /// <param name="highlighter">Active-query highlighter, or null when no query is set.</param>
    public void Fill(RowData data, IHighlighter? highlighter = null)
    {
        EntryRef = data.EntryRef;
        FullPath = data.FullPath;
        IsPlaceholder = false;
        Name = data.Name;
        ParentPath = data.ParentPath;
        SizeText = data.IsDirectory ? string.Empty : FormatSize(data.Size);
        DateText = data.Mtime > 0
            ? DateTimeOffset.FromFileTime(data.Mtime).ToLocalTime().ToString("yyyy/MM/dd HH:mm", CultureInfo.InvariantCulture)
            : string.Empty;
        Glyph = data.IsDirectory ? "" : ""; // Folder : Page
        ApplyHighlight(highlighter, data);
    }

    /// <summary>Compute this row's name/path highlight ranges for the active
    /// query. Path terms match the full path and are split at the parent/name
    /// boundary so each TextBlock gets only its own slice. Ranges are assigned
    /// only when they change, so a same-query RefreshInPlace refill (identical
    /// ranges) raises no notification and repaints nothing.</summary>
    /// <param name="highlighter">Active-query highlighter, or null when no query is set.</param>
    /// <param name="data">The engine page hit whose name and path are matched.</param>
    private void ApplyHighlight(IHighlighter? highlighter, RowData data)
    {
        if (highlighter is null || highlighter.IsEmpty)
        {
            AssignRanges(NoRanges, NoRanges);
            return;
        }

        var nameHits = new List<HighlightRange>(highlighter.Ranges(data.Name, HighlightField.Name));
        var parentHits = new List<HighlightRange>();
        var boundary = data.ParentPath.Length;
        foreach (var r in highlighter.Ranges(data.FullPath, HighlightField.Path))
        {
            SplitAtBoundary(r, boundary, parentHits, nameHits);
        }

        AssignRanges(
            ToShared(CompiledHighlighter.MergeRanges(nameHits)),
            ToShared(CompiledHighlighter.MergeRanges(parentHits)));
    }

    /// <summary>Assign the computed ranges, but only when they differ from the
    /// current ones — equal ranges keep the existing reference so the
    /// ObservableProperty setter stays silent (RefreshInPlace anti-flicker).</summary>
    /// <param name="name">Computed highlight ranges for the name.</param>
    /// <param name="path">Computed highlight ranges for the parent path.</param>
    private void AssignRanges(IReadOnlyList<HighlightRange> name, IReadOnlyList<HighlightRange> path)
    {
        if (!RangesEqual(NameRanges, name))
        {
            NameRanges = name;
        }

        if (!RangesEqual(PathRanges, path))
        {
            PathRanges = path;
        }
    }

    /// <summary>Split a full-path match at the parent/name boundary: the slice
    /// before <paramref name="boundary"/> highlights the parent path, the slice
    /// after highlights the name (re-based to name-local coordinates). A match
    /// straddling the separator lands in both.</summary>
    /// <param name="r">A full-path match range in full-path coordinates.</param>
    /// <param name="boundary">Index where the parent path ends and the name begins.</param>
    /// <param name="parent">Accumulates the parent-path slice of the match.</param>
    /// <param name="name">Accumulates the name slice, re-based to name-local coordinates.</param>
    private static void SplitAtBoundary(
        HighlightRange r, int boundary, List<HighlightRange> parent, List<HighlightRange> name)
    {
        var end = r.Start + r.Length;
        if (r.Start < boundary)
        {
            var leftEnd = Math.Min(end, boundary);
            parent.Add(new HighlightRange(r.Start, leftEnd - r.Start));
        }

        if (end > boundary)
        {
            var rightStart = Math.Max(r.Start, boundary);
            name.Add(new HighlightRange(rightStart - boundary, end - rightStart));
        }
    }

    private static IReadOnlyList<HighlightRange> ToShared(List<HighlightRange> list) =>
        list.Count == 0 ? NoRanges : list;

    private static bool RangesEqual(IReadOnlyList<HighlightRange> a, IReadOnlyList<HighlightRange> b)
    {
        if (ReferenceEquals(a, b))
        {
            return true;
        }

        if (a.Count != b.Count)
        {
            return false;
        }

        for (var i = 0; i < a.Count; i++)
        {
            if (a[i] != b[i])
            {
                return false;
            }
        }

        return true;
    }

    private static string FormatSize(ulong bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024.0:N0} KB",
        < 1024UL * 1024 * 1024 => $"{bytes / (1024.0 * 1024):N1} MB",
        _ => $"{bytes / (1024.0 * 1024 * 1024):N2} GB",
    };
}
