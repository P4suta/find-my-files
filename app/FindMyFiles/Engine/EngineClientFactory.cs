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
        FileLog.Info("app", "engine: in-proc FFI (pipe probe failed)");
        return new FfiEngineClient();
    }

    private static bool HasFlag(string[] args, string flag) =>
        args.Any(a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));

    private static string? OptionValue(string[] args, string prefix) =>
        args.FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];
}
