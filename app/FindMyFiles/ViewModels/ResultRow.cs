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
    public long Index { get; init; }

    [ObservableProperty]
    public partial string Name { get; set; } = string.Empty;

    [ObservableProperty]
    public partial string ParentPath { get; set; } = string.Empty;

    [ObservableProperty]
    public partial string SizeText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial string DateText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial string Glyph { get; set; } = ""; // Page

    public string FullPath { get; private set; } = string.Empty;
    public bool IsPlaceholder { get; private set; } = true;

    public static ResultRow CreatePlaceholder(long index) => new() { Index = index };

    public void Fill(RowData data)
    {
        FullPath = data.FullPath;
        IsPlaceholder = false;
        Name = data.Name;
        ParentPath = data.ParentPath;
        SizeText = data.IsDirectory ? string.Empty : FormatSize(data.Size);
        DateText = data.Mtime > 0
            ? DateTimeOffset.FromFileTime(data.Mtime).ToLocalTime().ToString("yyyy/MM/dd HH:mm")
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
