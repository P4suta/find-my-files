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
        if (IsElevated())
        {
            FileLog.Info("app", "engine: in-proc FFI (pipe probe failed, process is elevated)");
            return new FfiEngineClient();
        }
        // ARCHITECTURE.md エンジン選択の契約: サービス不在+非昇格で in-proc を
        // 作っても MFT 読みで必ず失敗する(原因を語らない「インデックスに失敗」
        // になる)。Fake に劣化し、理由と出口 — サービス導入(恒久)または明示の
        // 昇格再起動 — を提示する。自動 runas ループは禁止。
        FileLog.Warn("app", "engine: fake fallback (no service answered, not elevated)");
        Notifier.Post(
            NotifySeverity.Warning,
            "検索サービスが見つからないため、デモデータで起動しました",
            "実ファイルを検索するには、管理者ターミナルで一度 `just service-install` を実行して"
            + "サービスを登録してください(以後は通常起動で動きます)。"
            + "今すぐ試す場合は右のボタンで管理者として再起動できます。",
            actionLabel: "管理者として再起動",
            action: () => ShellOps.RestartElevated(args));
        return new FakeEngineClient();
    }

    private static bool IsElevated()
    {
        using var identity = System.Security.Principal.WindowsIdentity.GetCurrent();
        return new System.Security.Principal.WindowsPrincipal(identity)
            .IsInRole(System.Security.Principal.WindowsBuiltInRole.Administrator);
    }

    private static bool HasFlag(string[] args, string flag) =>
        args.Any(a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));

    private static string? OptionValue(string[] args, string prefix) =>
        args.FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];
}
