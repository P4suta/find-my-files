using Serilog;
using Serilog.Core;
using Serilog.Events;

namespace FindMyFiles.Services;

/// <summary>
/// One-time Serilog bootstrap for the app process: a logfmt <c>app.log</c> File
/// sink under <see cref="AppPaths.LogDir"/> with a size cap and multi-generation
/// retention, plus <c>LogContext</c> enrichment. Mirrors the engine's
/// diagnostics home (ADR-0037); the <see cref="FileLog"/> facade routes through
/// the logger installed here.
/// </summary>
public static class LogSetup
{
    /// <summary>Active-file size before rolling to <c>app_NNN.log</c>. Larger
    /// than the old 2 MiB because a logfmt line is more verbose.</summary>
    private const long FileSizeLimitBytes = 5L * 1024 * 1024;

    /// <summary>Retained generations (active + rolled) — was a single
    /// <c>.old</c>; five gives a useful tail without unbounded growth.</summary>
    private const int RetainedFiles = 5;

    private static int _initialized;

    /// <summary>Runtime-adjustable level gate — the C# analogue of the engine's
    /// <c>FMF_LOG</c>. Seeded from that same variable at <see cref="Init"/>;
    /// callers may lower it at runtime (e.g. from a diagnostics toggle).</summary>
    public static LoggingLevelSwitch LevelSwitch { get; } = new(LogEventLevel.Information);

    /// <summary>Install the global logger once, before any <see cref="FileLog"/>
    /// use. Idempotent. Reads the initial level from the <c>FMF_LOG</c>
    /// environment variable (the spelling shared with the engine).</summary>
    public static void Init()
    {
        if (Interlocked.Exchange(ref _initialized, 1) != 0)
        {
            return;
        }

        LevelSwitch.MinimumLevel = LevelFromEnv();

        Log.Logger = new LoggerConfiguration()
            .MinimumLevel.ControlledBy(LevelSwitch)
            .Enrich.FromLogContext()
            .WriteTo.File(
                formatter: new LogfmtFormatter(),
                path: Path.Combine(AppPaths.LogDir, "app.log"),
                fileSizeLimitBytes: FileSizeLimitBytes,
                rollingInterval: RollingInterval.Infinite,
                rollOnFileSizeLimit: true,
                retainedFileCountLimit: RetainedFiles,
                shared: false,
                buffered: false)
            .CreateLogger();
    }

    /// <summary>Flush and close the logger; call once on shutdown so the last
    /// lines reach disk.</summary>
    public static void Shutdown() => Log.CloseAndFlush();

    private static LogEventLevel LevelFromEnv()
    {
        var raw = Environment.GetEnvironmentVariable("FMF_LOG");
        if (string.IsNullOrWhiteSpace(raw))
        {
            return LogEventLevel.Information;
        }

        // Accept a bare level word; an EnvFilter-style directive (the engine's
        // richer syntax) degrades gracefully to a sensible coarse level.
        return raw.Trim().ToUpperInvariant() switch
        {
            "TRACE" => LogEventLevel.Verbose,
            "DEBUG" => LogEventLevel.Debug,
            "INFO" or "INFORMATION" => LogEventLevel.Information,
            "WARN" or "WARNING" => LogEventLevel.Warning,
            "ERROR" => LogEventLevel.Error,
            _ => LogEventLevel.Information,
        };
    }
}
