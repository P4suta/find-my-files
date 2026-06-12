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

    public static IEngineClient Resolve(string[] args)
    {
        if (HasFlag(args, "--fake-engine"))
        {
            FileLog.Info("app", "engine: fake (--fake-engine)");
            return new FakeEngineClient();
        }
        var pipeName = OptionValue(args, "--pipe-name=") ?? PipeProtocol.DefaultPipeName;
        var mode = OptionValue(args, "--engine=") ?? AppSettings.Load().Engine;
        switch (mode.ToLowerInvariant())
        {
            case "pipe":
                FileLog.Info("app", $"engine: pipe ({pipeName})");
                return new PipeEngineClient(pipeName);
            case "inproc":
                FileLog.Info("app", "engine: in-proc FFI (explicit)");
                return new FfiEngineClient();
            case "auto":
                break;
            default:
                FileLog.Warn(
                    "app",
                    $"unknown engine mode `{mode}` (allowed: pipe | inproc | auto) — using auto");
                break;
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
            Notifier.Post(
                NotifySeverity.Warning,
                "検索サービスは稼働中ですが、接続を許可されていません",
                "「管理者として再起動」してアプリ内の「サービスを登録し直す」を一度押すと、"
                + "このユーザーが接続できるようになります。",
                actionLabel: "管理者として再起動",
                action: () => ShellOps.RestartElevated(ElevationArgs(args)));
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
        // アプリで偽データに実用性はない)、理由と出口 — 明示の昇格再起動 →
        // アプリ内サービス登録 — を提示する。自動 runas ループは禁止。
        FileLog.Warn("app", "engine: empty fallback (no service answered, not elevated)");
        Notifier.Post(
            NotifySeverity.Warning,
            "検索サービスに接続できません",
            "右のボタンで管理者として再起動し、表示される「サービスを登録して開始」を"
            + "一度押してください。以後は通常起動(ダブルクリック)のまま使えます。",
            actionLabel: "管理者として再起動",
            action: () => ShellOps.RestartElevated(ElevationArgs(args)));
        return FakeEngineClient.CreateEmpty();
    }

    private static bool IsElevated() => ServiceSetup.IsProcessElevated();

    /// <summary>The current args plus <c>--setup-owner=&lt;sid&gt;</c> (when
    /// resolvable) so the elevated relaunch can forward this user's SID to
    /// <c>fmf-service install</c> — under OTS elevation the elevated process
    /// runs as a different admin and would otherwise authorize only that
    /// account (ARCHITECTURE.md 脅威1).</summary>
    private static string[] ElevationArgs(string[] args)
    {
        var sid = ServiceSetup.CurrentUserSid();
        return ServiceSetup.IsValidSid(sid) ? [.. args, $"--setup-owner={sid}"] : args;
    }

    private static bool HasFlag(string[] args, string flag) =>
        args.Any(a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));

    private static string? OptionValue(string[] args, string prefix) =>
        args.FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];
}
