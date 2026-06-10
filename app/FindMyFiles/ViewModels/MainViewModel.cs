using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;
using FindMyFiles.Engine;
using FindMyFiles.Virtualization;

namespace FindMyFiles.ViewModels;

/// <summary>
/// Search pipeline: TextChanged → 50ms debounce → engine query → swap the
/// virtualized ItemsSource, with a generation counter discarding stale
/// responses (docs/ARCHITECTURE.md).
/// </summary>
public sealed partial class MainViewModel : ObservableObject
{
    private readonly IEngineClient _engine;
    private readonly DispatcherQueue _dispatcher;
    private readonly DispatcherQueueTimer _debounce;
    private long _generation;

    [ObservableProperty]
    public partial string SearchText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial VirtualResultList? ResultsSource { get; set; }

    [ObservableProperty]
    public partial string StatusText { get; set; } = "準備中…";

    [ObservableProperty]
    public partial string CountText { get; set; } = string.Empty;

    [ObservableProperty]
    public partial FmfSort Sort { get; set; } = FmfSort.Name;

    [ObservableProperty]
    public partial bool SortDescending { get; set; }

    public MainViewModel(IEngineClient engine, DispatcherQueue dispatcher)
    {
        _engine = engine;
        _dispatcher = dispatcher;
        _debounce = dispatcher.CreateTimer();
        _debounce.Interval = TimeSpan.FromMilliseconds(50);
        _debounce.IsRepeating = false;
        _debounce.Tick += (_, _) => _ = RunQueryAsync();

        _engine.IndexChanged += volume => _dispatcher.TryEnqueue(() => _ = RunQueryAsync());
        _engine.VolumeUpdated += s => _dispatcher.TryEnqueue(() => OnVolumeUpdated(s));
    }

    public void Start()
    {
        var volumes = _engine.ListVolumes();
        StatusText = volumes.Count == 0
            ? "NTFS固定ドライブが見つかりません"
            : $"インデックス作成中: {string.Join(", ", volumes)}";
        _engine.StartIndexing(volumes);
        _ = RunQueryAsync();
    }

    partial void OnSearchTextChanged(string value)
    {
        _debounce.Stop();
        if (string.IsNullOrEmpty(value))
        {
            _ = RunQueryAsync(); // clearing should feel instant
        }
        else
        {
            _debounce.Start();
        }
    }

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
        _ = RunQueryAsync();
    }

    private void OnVolumeUpdated(VolumeStatus s)
    {
        StatusText = s.State switch
        {
            VolumeState.Scanning => $"{s.Label} をインデックス中… {s.Entries:N0} 件",
            VolumeState.Ready => $"{s.Label} 準備完了 — {s.Entries:N0} 件",
            VolumeState.Rescanning => $"{s.Label} を再スキャン中…",
            VolumeState.Failed => $"{s.Label} のインデックスに失敗",
            _ => StatusText,
        };
        if (s.State == VolumeState.Ready)
        {
            _ = RunQueryAsync();
        }
    }

    private async Task RunQueryAsync()
    {
        var generation = Interlocked.Increment(ref _generation);
        var query = SearchText;
        var options = new SearchOptions(Sort, SortDescending, FmfCase.Smart);
        try
        {
            var result = await _engine.SearchAsync(query, options);
            if (generation != Interlocked.Read(ref _generation))
            {
                result.Dispose(); // a newer query superseded this one
                return;
            }
            var old = ResultsSource;
            var list = new VirtualResultList(result, _dispatcher);
            list.BecameStale += () => _ = RunQueryAsync();
            ResultsSource = list;
            old?.Dispose();
            CountText = $"{result.Count:N0} 件";
        }
        catch (QuerySyntaxException e)
        {
            CountText = $"クエリエラー: {e.Message}";
        }
        catch (EngineException e)
        {
            CountText = string.Empty;
            StatusText = $"エンジンエラー: {e.Message}";
        }
    }
}
