using Microsoft.UI.Xaml;

namespace FindMyFiles.Services;

/// <summary>
/// The single home for the process-wide exception funnels
/// (「落ちない・固まらない・黙らない」). Three sources, three suppression
/// rules:
///
/// 1. XAML <see cref="Application.UnhandledException"/> — suppressed
///    (<c>Handled = true</c>) and surfaced as an error InfoBar for the first
///    <see cref="XamlStormBudget"/> occurrences. Beyond that the process is
///    in an exception storm: a crash marker is written and the exception is
///    left unhandled so the process dies honestly.
/// 2. <see cref="AppDomain.UnhandledException"/> — never suppressible by
///    contract: log + crash marker, then the runtime terminates.
/// 3. <see cref="TaskScheduler.UnobservedTaskException"/> — always observed
///    (the task is already dead; tearing the process down helps nobody) and
///    surfaced as an error InfoBar.
///
/// Log routing: every path writes to <see cref="FileLog"/>
/// (%APPDATA%\find-my-files\logs\app.log); user-visible surfaces go through
/// <see cref="Notifier"/>, which mirrors to the same log. Crash markers are
/// written by the fatal paths (1-storm and 2) and read back + cleared by
/// <see cref="ReportPreviousCrash"/> on the next launch, so no crash is ever
/// silent.
/// </summary>
public sealed class ExceptionPolicy
{
    /// <summary>XAML exceptions suppressed before declaring a storm.</summary>
    internal const int XamlStormBudget = 3;

    private int _xamlStorm;

    private ExceptionPolicy()
    {
    }

    /// <summary>Wire all three funnels. Call once, from the App constructor.</summary>
    /// <param name="app">The WinUI application whose exception events are hooked.</param>
    public static void Install(Application app)
    {
        var policy = new ExceptionPolicy();
        app.UnhandledException += policy.OnXamlUnhandledException;
        AppDomain.CurrentDomain.UnhandledException += policy.OnAppDomainUnhandledException;
        TaskScheduler.UnobservedTaskException += policy.OnUnobservedTaskException;
    }

    /// <summary>Suppression rule for funnel 1, as a pure predicate.</summary>
    /// <param name="occurrence">The 1-based count of XAML exceptions seen so far.</param>
    /// <returns><c>true</c> while still within the storm budget (suppress); <c>false</c> once the storm threshold is exceeded.</returns>
    internal static bool WithinStormBudget(int occurrence) => occurrence <= XamlStormBudget;

    private void OnXamlUnhandledException(
        object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        FileLog.Error("xaml", "unhandled exception", e.Exception);
        if (WithinStormBudget(System.Threading.Interlocked.Increment(ref _xamlStorm)))
        {
            e.Handled = true;
            Notifier.Post(
                NotifySeverity.Error,
                Loc.Get("Crash_UnexpectedTitle"),
                e.Exception?.Message);
        }
        else
        {
            // Exception storm — record and let the process die honestly.
            FileLog.WriteCrashMarker(e.Exception?.ToString() ?? "exception storm");
        }
    }

    private void OnAppDomainUnhandledException(object sender, System.UnhandledExceptionEventArgs e)
    {
        var ex = e.ExceptionObject as Exception;
        FileLog.Error("appdomain", "fatal unhandled exception", ex);
        FileLog.WriteCrashMarker(ex?.ToString() ?? "unknown fatal exception");
    }

    private void OnUnobservedTaskException(object? sender, UnobservedTaskExceptionEventArgs e)
    {
        FileLog.Error("task", "unobserved task exception", e.Exception);
        e.SetObserved();
        Notifier.Post(
            NotifySeverity.Error,
            Loc.Get("Crash_BackgroundTitle"),
            e.Exception.InnerException?.Message ?? e.Exception.Message);
    }

    /// <summary>Surface the previous run's crash marker (if any) as a warning
    /// InfoBar; reading clears the marker.</summary>
    public static void ReportPreviousCrash()
    {
        if (FileLog.TakeCrashMarker() is { } marker)
        {
            FileLog.Warn("app", "previous run crashed");
            Notifier.Post(
                NotifySeverity.Warning,
                Loc.Get("Crash_PreviousTitle"),
                Loc.Get("Crash_PreviousBody", FileLog.LogPath, marker.Split('\n').FirstOrDefault() ?? string.Empty));
        }
    }
}
