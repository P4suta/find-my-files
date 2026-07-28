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
internal static class LogSetup
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
        Log.Logger = CreateLogger(
            AppPaths.LogDir,
            message => System.Diagnostics.Debug.WriteLine(message));
    }

    /// <summary>Build the file logger, falling back to a no-sink logger when
    /// diagnostics storage is unavailable. Logging must never block app start.</summary>
    /// <param name="logDir">Directory that owns <c>app.log</c>.</param>
    /// <param name="reportFailure">Optional privacy-safe failure observer.</param>
    /// <returns>A usable logger in both the file and degraded cases.</returns>
    internal static Logger CreateLogger(string logDir, Action<string>? reportFailure = null)
    {
        try
        {
            Directory.CreateDirectory(logDir);
            return new LoggerConfiguration()
                .MinimumLevel.ControlledBy(LevelSwitch)
                .Enrich.FromLogContext()
                .WriteTo.File(
                    formatter: new LogfmtFormatter(),
                    path: Path.Combine(logDir, "app.log"),
                    fileSizeLimitBytes: FileSizeLimitBytes,
                    rollingInterval: RollingInterval.Infinite,
                    rollOnFileSizeLimit: true,
                    retainedFileCountLimit: RetainedFiles,
                    shared: false,
                    buffered: false)
                .CreateLogger();
        }
        catch (Exception ex)
        {
            // Diagnostics are a support feature, never an app-start precondition.
            // Do not include Message/paths: Debug output can be collected too.
            reportFailure?.Invoke(
                $"FindMyFiles log initialization failed: {ex.GetType().FullName} 0x{ex.HResult:X8}");
            return new LoggerConfiguration()
                .MinimumLevel.ControlledBy(LevelSwitch)
                .CreateLogger();
        }
    }

    /// <summary>Flush and close the logger. Reset the initialization guard so a
    /// failed full-purge attempt can reopen diagnostics before the app continues.
    /// Repeated shutdown remains safe.</summary>
    public static void Shutdown()
    {
        Interlocked.Exchange(ref _initialized, 0);
        Log.CloseAndFlush();
    }

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
