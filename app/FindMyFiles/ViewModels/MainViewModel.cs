using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// Composition root of the main page: owns the UI state (search text, sort,
/// filter, status line) and the user-facing wording for failures, and wires
/// the focused components together — <see cref="SearchOrchestrator"/> (when
/// to search), <see cref="ResultsPresenter"/> (how results land),
/// <see cref="NotificationCenter"/> (InfoBar stack) and
/// <see cref="PerfPanelViewModel"/> (F12).
/// </summary>
public sealed partial class MainViewModel : ObservableObject
{
    private readonly IEngineClient _engine;

    [ObservableProperty]
    public partial string SearchText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial string StatusText { get; set; } = "準備中…";

    [ObservableProperty]
    public partial FmfSort Sort { get; set; } = FmfSort.Name;

    [ObservableProperty]
    public partial bool SortDescending { get; set; }

    [ObservableProperty]
    public partial bool IncludeHiddenSystem { get; set; }

    public ResultsPresenter Results { get; }
    public SearchOrchestrator Search { get; }
    public NotificationCenter Notifications { get; }
    public PerfPanelViewModel Perf { get; }

    public MainViewModel(IEngineClient engine, IDispatcher dispatcher)
    {
        _engine = engine;
        Results = new ResultsPresenter(dispatcher);
        Search = new SearchOrchestrator(engine, dispatcher, Results, () => new SearchRequest(
            SearchText,
            new SearchOptions(Sort, SortDescending, FmfCase.Smart, IncludeHiddenSystem)));
        Notifications = new NotificationCenter(dispatcher);
        Perf = new PerfPanelViewModel(engine);

        Search.TraceCaptured += Perf.RecordTrace;
        Search.SearchFailed += OnSearchFailed;

        _engine.VolumeUpdated += s => dispatcher.TryEnqueue(() => OnVolumeUpdated(s));
        _engine.EngineErrorOccurred += severity =>
            dispatcher.TryEnqueue(() =>
                HandleEngineErrorAsync(severity).Forget("engine.error"));

        Notifications.AttachToNotifier();
    }

    /// <summary>Startup sequence, in order: status text → StartIndexing →
    /// initial requery. Runs on the UI thread; the engine calls are awaited
    /// so a pipe transport never blocks it.</summary>
    public async Task StartAsync()
    {
        try
        {
            var volumes = await _engine.ListVolumesAsync();
            StatusText = StatusFormatter.IndexingStarted(volumes);
            await _engine.StartIndexingAsync(volumes);
        }
        catch (Exception ex)
        {
            FileLog.Error("engine", "startup indexing failed", ex);
            StatusText = "インデックス開始に失敗しました";
            Notifications.Push(new AppNotification(
                NotifySeverity.Error, "インデックスを開始できませんでした", ex.Message));
        }
        Search.Requery(RequeryOrigin.Initial);
    }

    partial void OnSearchTextChanged(string value) => Search.NotifyTextChanged(value);

    partial void OnIncludeHiddenSystemChanged(bool value) =>
        Search.Requery(RequeryOrigin.Filter);

    public void SetSort(FmfSort key)
    {
        if (Sort == key)
        {
            SortDescending = !SortDescending;
        }
        else
        {
            Sort = key;
            SortDescending = false;
        }
        Search.Requery(RequeryOrigin.Sort);
    }

    private void OnVolumeUpdated(VolumeStatus s)
    {
        StatusText = StatusFormatter.Volume(s, StatusText);
        if (s.State == VolumeState.Failed)
        {
            Notifications.Push(new AppNotification(
                NotifySeverity.Error,
                $"{s.Label} のインデックスに失敗しました",
                "詳細は F12 パネルまたは engine.log を参照"));
        }
        if (s.State == VolumeState.Ready)
        {
            Search.Requery(RequeryOrigin.VolumeReady);
        }
    }

    private void OnSearchFailed(Exception e) =>
        Notifications.Push(new AppNotification(
            NotifySeverity.Error,
            e is EngineException ? "検索に失敗しました" : "検索中に予期しないエラーが発生しました",
            e.Message));

    /// <summary>Engine diagnostics: pull the detail text behind the POD event.</summary>
    private async Task HandleEngineErrorAsync(int severity)
    {
        await Perf.RefreshStatsAsync();
        if (severity >= 2)
        {
            var last = Perf.Stats?.RecentErrors.LastOrDefault();
            Notifications.Push(new AppNotification(
                NotifySeverity.Error,
                severity >= 3 ? "エンジン内部でパニックが発生しました" : "エンジンでエラーが発生しました",
                last is null ? null : $"[{last.Area}] {Truncate(last.Message, 200)}"));
        }
    }

    private static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..max] + "…";
}
