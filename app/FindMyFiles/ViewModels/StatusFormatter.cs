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

    /// <summary>Startup/refresh snapshot of the overall index state — reflects
    /// whatever the engine reports right now, so an already-Ready volume never
    /// shows "indexing…". <paramref name="requested"/> is the list we asked to
    /// index, used only for messaging when the engine hasn't surfaced any
    /// status yet.</summary>
    public static string Overall(
        IReadOnlyList<VolumeStatus> volumes, IReadOnlyList<string> requested)
    {
        if (volumes.Count == 0)
        {
            return requested.Count == 0
                ? "NTFS固定ドライブが見つかりません"
                : $"インデックス作成中: {string.Join(", ", requested)}";
        }
        var pending = volumes
            .Where(v => v.State is VolumeState.Scanning or VolumeState.Rescanning)
            .Select(v => v.Label)
            .ToList();
        if (pending.Count > 0)
        {
            return $"インデックス作成中: {string.Join(", ", pending)}";
        }
        if (volumes.All(v => v.State == VolumeState.Failed))
        {
            return $"インデックスに失敗: {string.Join(", ", volumes.Select(v => v.Label))}";
        }
        var total = volumes.Where(v => v.State == VolumeState.Ready).Sum(v => (long)v.Entries);
        return $"準備完了 — {total:N0} 件";
    }

    /// <summary>Status-bar transport badge: which engine the app talks to
    /// right now (client type + live connection state).</summary>
    public static string EngineMode(IEngineClient engine) => engine switch
    {
        FakeEngineClient { IsEmpty: true } => "未接続",
        FakeEngineClient => "fake",
        FfiEngineClient => "管理者(in-proc)",
        PipeEngineClient => engine.Connection switch
        {
            EngineConnectionState.Connected => "サービス接続",
            EngineConnectionState.Reconnecting => "再接続中…",
            _ => "接続中…",
        },
        _ => string.Empty,
    };

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
