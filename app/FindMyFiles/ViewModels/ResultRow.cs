using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;

namespace FindMyFiles.ViewModels;

/// <summary>
/// One list row. Created as a placeholder by the virtualized list and filled
/// in place when its page arrives — the same instance stays bound, so no
/// container regeneration happens (docs/ARCHITECTURE.md).
/// </summary>
public sealed partial class ResultRow : ObservableObject
{
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

    /// <summary>Absolute path (<c>ParentPath + Name</c>) — what 「開く」/コピー act on.
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
    public static ResultRow CreatePlaceholder(long index) => new() { Index = index };

    /// <summary>Populate this placeholder from an engine <see cref="RowData"/>
    /// page hit: copies identity (<see cref="EntryRef"/>, <see cref="FullPath"/>),
    /// formats size/date for display, picks the type glyph, and clears
    /// <see cref="IsPlaceholder"/>. In place — the bound instance is reused.</summary>
    public void Fill(RowData data)
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
    }

    private static string FormatSize(ulong bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024.0:N0} KB",
        < 1024UL * 1024 * 1024 => $"{bytes / (1024.0 * 1024):N1} MB",
        _ => $"{bytes / (1024.0 * 1024 * 1024):N2} GB",
    };
}
