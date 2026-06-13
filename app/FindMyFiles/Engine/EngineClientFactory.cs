using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// Engine transport selection, in priority order: CLI flags (--fake-engine /
/// --engine=pipe|inproc / --pipe-name=…) > settings.json "engine" > auto.
/// Auto probes the service pipe for 250ms (through Hello) and falls back to
/// the in-proc FFI engine when no service answers.
/// </summary>
public static class EngineClientFactory
{
    private static readonly TimeSpan ProbeTimeout = TimeSpan.FromMilliseconds(250);

    /// <summary>起動時に一度だけ呼び、上記の優先順位でエンジン実装を 1 つ
    /// 解決して返す。サービス不在+非昇格など in-proc が使えない状況では
    /// 空エンジン(<see cref="FakeEngineClient.CreateEmpty"/>)に劣化させ、
    /// UI を setup 画面に誘導する(自動 runas はしない)。</summary>
    /// <param name="args">プロセスのコマンドライン引数(`--fake-engine` /
    /// `--engine=` / `--pipe-name=` を見る)。</param>
    /// <returns>選択された <see cref="IEngineClient"/> 実装の単一インスタンス。</returns>
    public static IEngineClient Resolve(string[] args)
    {
        if (HasFlag(args, "--fake-engine"))
        {
            FileLog.Info("app", "engine: fake (--fake-engine)");
            return new FakeEngineClient();
        }
        var pipeName = OptionValue(args, "--pipe-name=") ?? PipeProtocol.DefaultPipeName;
        var mode = OptionValue(args, "--engine=") ?? AppSettings.Load().Engine;
        if (string.Equals(mode, "pipe", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Info("app", $"engine: pipe ({pipeName})");
            return new PipeEngineClient(pipeName);
        }
        if (string.Equals(mode, "inproc", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Info("app", "engine: in-proc FFI (explicit)");
            return new FfiEngineClient();
        }
        if (!string.Equals(mode, "auto", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Warn(
                "app",
                $"unknown engine mode `{mode}` (allowed: pipe | inproc | auto) — using auto");
        }
        if (PipeEngineClient.Probe(pipeName, ProbeTimeout))
        {
            FileLog.Info("app", $"engine: pipe ({pipeName}, probe succeeded)");
            return new PipeEngineClient(pipeName);
        }

        // Probe failed — but "no service" and "service running yet rejecting
        // us" need opposite responses. A running service holds the writer
        // lock, so in-proc would die FMF_E_LOCKED (the「初期化に失敗」path);
        // only an absent/stopped service leaves the lock free for in-proc.
        if (ServiceSetup.QueryState() == EngineServiceState.Running)
        {
            // Serving, but our token isn't on its authorized-SID list (a
            // stale list baked in at service startup, or a foreign installer
            // SID). In-proc is off the table; recovery is to re-register so
            // this user's SID is applied and the service restarted.
            FileLog.Warn(
                "app", "engine: service running but unreachable (token rejected) — empty fallback");
            // The setup screen (MainViewModel.IsDisconnected) owns the recovery
            // path (one-click 登録し直す); no separate notification.
            return FakeEngineClient.CreateEmpty();
        }

        // Service absent or stopped → the writer lock is free for in-proc.
        if (IsElevated())
        {
            FileLog.Info("app", "engine: in-proc FFI (no live service, process is elevated)");
            return new FfiEngineClient();
        }
        // ARCHITECTURE.md エンジン選択の契約: サービス不在+非昇格で in-proc を
        // 作っても MFT 読みで必ず失敗する(原因を語らない「インデックスに失敗」
        // になる)。結果ゼロの空エンジンに劣化し(デモデータは出さない — 検索
        // アプリで偽データに実用性はない)、UI はこの空エンジンを検知して
        // セットアップ画面(MainViewModel.IsDisconnected)に切り替え、ワンクリック
        // 登録(操作ごとに UAC、アプリは非昇格のまま)を提示する。自動 runas 禁止。
        FileLog.Warn("app", "engine: empty fallback (no service answered, not elevated)");
        return FakeEngineClient.CreateEmpty();
    }

    private static bool IsElevated() => ServiceSetup.IsProcessElevated();

    private static bool HasFlag(string[] args, string flag) =>
        args.Any(a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));

    private static string? OptionValue(string[] args, string prefix) =>
        args.FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];
}
