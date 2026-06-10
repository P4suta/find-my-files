using FindMyFiles.Engine;

namespace FindMyFiles.ViewModels;

/// <summary>All user-facing status wording in one pure, testable place.</summary>
public static class StatusFormatter
{
    public static string Count(QueryTraceData? trace, long hits) =>
        trace is { } t
            ? $"{t.TotalUs / 1000.0:F1} ms · {hits:N0} 件"
            : $"{hits:N0} 件";

    public static string QueryError(string message) => $"クエリエラー: {message}";

    public static string IndexingStarted(IReadOnlyList<string> volumes) =>
        volumes.Count == 0
            ? "NTFS固定ドライブが見つかりません"
            : $"インデックス作成中: {string.Join(", ", volumes)}";

    /// <summary>Status-bar line for a volume state change; falls back to the
    /// current text for states that carry no message.</summary>
    public static string Volume(VolumeStatus s, string current) => s.State switch
    {
        VolumeState.Scanning => $"{s.Label} をインデックス中… {s.Entries:N0} 件",
        VolumeState.Ready => $"{s.Label} 準備完了 — {s.Entries:N0} 件",
        VolumeState.Rescanning => $"{s.Label} を再スキャン中…",
        VolumeState.Failed => $"{s.Label} のインデックスに失敗",
        _ => current,
    };
}
