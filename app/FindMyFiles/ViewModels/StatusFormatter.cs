using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>All user-facing status wording in one place — keys resolve through
/// <see cref="Loc"/> (Strings/&lt;lang&gt;/Resources.resw).</summary>
public static class StatusFormatter
{
    /// <summary>Result-count line: "<paramref name="hits"/> 件" plus the
    /// elapsed query time (ms) when a <paramref name="trace"/> is present
    /// (<see cref="QueryTraceData.TotalUs"/> → ms), the bare count otherwise.</summary>
    /// <param name="trace">Latest query trace, or null when timing is unavailable.</param>
    /// <param name="hits">Number of matching results.</param>
    /// <returns>Localized count line, with elapsed time when a trace is present.</returns>
    public static string Count(QueryTraceData? trace, long hits) =>
        trace is { } t
            ? Loc.Get("Status_CountWithTime", t.TotalUs / 1000.0, hits)
            : Loc.Get("Status_Count", hits);

    /// <summary>Status line for a rejected query — the engine's syntax-error
    /// <paramref name="message"/> behind a localized prefix.</summary>
    /// <param name="message">Engine syntax-error message for the rejected query.</param>
    /// <returns>Localized error line with the engine message behind a prefix.</returns>
    public static string QueryError(string message) => Loc.Get("Status_QueryErrorPrefix", message);

    /// <summary>Startup/refresh snapshot of the overall index state — reflects
    /// whatever the engine reports right now, so an already-Ready volume never
    /// shows "indexing…". <paramref name="requested"/> is the list we asked to
    /// index, used only for messaging when the engine hasn't surfaced any
    /// status yet.</summary>
    /// <param name="volumes">Per-volume status the engine reports right now.</param>
    /// <param name="requested">Volumes we asked to index, used only for early messaging.</param>
    /// <returns>Localized overall index-state line.</returns>
    public static string Overall(
        IReadOnlyList<VolumeStatus> volumes, IReadOnlyList<string> requested)
    {
        if (volumes.Count == 0)
        {
            return requested.Count == 0
                ? Loc.Get("Status_NoNtfsDrives")
                : Loc.Get("Status_Indexing", string.Join(", ", requested));
        }

        var pending = volumes
            .Where(v => v.State is VolumeState.Scanning or VolumeState.Rescanning)
            .Select(v => v.Label)
            .ToList();
        if (pending.Count > 0)
        {
            return Loc.Get("Status_Indexing", string.Join(", ", pending));
        }

        if (volumes.All(v => v.State == VolumeState.Failed))
        {
            return Loc.Get("Status_IndexFailed", string.Join(", ", volumes.Select(v => v.Label)));
        }

        var total = volumes.Where(v => v.State == VolumeState.Ready).Sum(v => (long)v.Entries);
        return Loc.Get("Status_Ready", total);
    }

    /// <summary>Status-bar transport badge: which engine the app talks to
    /// right now (client type + live connection state).</summary>
    /// <param name="engine">Engine client whose type and connection state to describe.</param>
    /// <returns>Localized transport badge for the active engine.</returns>
    public static string EngineMode(IEngineClient engine) => engine switch
    {
        FakeEngineClient { IsEmpty: true } => Loc.Get("EngineMode_Disconnected"),
        FakeEngineClient => Loc.Get("EngineMode_Fake"),
        FfiEngineClient => Loc.Get("EngineMode_Admin"),
        PipeEngineClient => engine.Connection switch
        {
            EngineConnectionState.Connected => Loc.Get("EngineMode_Connected"),
            EngineConnectionState.Reconnecting => Loc.Get("EngineMode_Reconnecting"),
            _ => Loc.Get("EngineMode_Connecting"),
        },
        _ => string.Empty,
    };

    /// <summary>Status-bar line for a volume state change; falls back to the
    /// current text for states that carry no message.</summary>
    /// <param name="s">Volume status that changed.</param>
    /// <param name="current">Existing status text, returned for states with no message.</param>
    /// <returns>Localized line for the new volume state, or <paramref name="current"/>.</returns>
    public static string Volume(VolumeStatus s, string current) => s.State switch
    {
        VolumeState.Scanning => Loc.Get("Volume_Indexing", s.Label, s.Entries),
        VolumeState.Ready => Loc.Get("Volume_Ready", s.Label, s.Entries),
        VolumeState.Rescanning => Loc.Get("Volume_Rescanning", s.Label),
        VolumeState.Failed => Loc.Get("Volume_Failed", s.Label),
        _ => current,
    };
}
