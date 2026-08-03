using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.Windows.AppLifecycle;

namespace FindMyFiles.Services;

/// <summary>
/// Shell-facing operations, centralized so every failure path notifies the
/// user instead of crashing. Targets launch via explorer.exe to shed the
/// process's elevation (AGENTS.md UI rules).
/// </summary>
internal static partial class ShellOps
{
    /// <summary>Longest path <c>CreateFileW</c> accepts without long-path support:
    /// <c>MAX_PATH</c> (260) counts the terminating NUL.</summary>
    private const int LegacyMaxPathChars = 259;

    /// <summary>Full path to explorer.exe (<c>%WINDIR%\explorer.exe</c>).
    /// Launching by bare name under <c>UseShellExecute=false</c> lets
    /// CreateProcess search the current directory first — a binary-planting
    /// vector. Pin it to the Windows directory.</summary>
    private static readonly string ExplorerPath =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe");

    private static ShellOpsHooks _hooks = ShellOpsHooks.Production;

    /// <summary>Replaces every operating-system boundary for one deterministic test scope.</summary>
    /// <param name="hooks">Complete boundary implementation.</param>
    /// <returns>A scope that restores the previous implementation.</returns>
    internal static IDisposable UseHooksForTests(ShellOpsHooks hooks)
    {
        ArgumentNullException.ThrowIfNull(hooks);
        var previous = Interlocked.Exchange(ref _hooks, hooks);
        return new ActionOnDispose(() => Interlocked.Exchange(ref _hooks, previous));
    }

    /// <summary>Open a file or folder with its default handler via
    /// explorer.exe, shedding the app's elevation. Failures notify the user
    /// (with a Win32-specific hint) rather than throwing.</summary>
    /// <param name="fullPath">Absolute path to open; treated as data, never as
    /// a command line (see <see cref="BuildOpenStartInfo"/>).</param>
    public static void OpenTrusted(string fullPath) =>
        OpenTrustedWith(Volatile.Read(ref _hooks).ProcessRunner, fullPath);

    /// <summary>
    /// Open an MFT-sourced result only after the path has been pinned and its
    /// handle identity has been matched to the exact engine FRN.
    /// </summary>
    /// <param name="fullPath">MFT-sourced absolute path.</param>
    /// <param name="expectedFrn">Exact NTFS record-and-sequence identity.</param>
    public static void OpenIndexed(string fullPath, ulong expectedFrn)
    {
        var hooks = Volatile.Read(ref _hooks);
        OpenIndexedWith(hooks.Verifier, hooks.ProcessRunner, fullPath, expectedFrn);
    }

    /// <summary>"Open" core, parameterised over the process runner so the launch
    /// (not just <see cref="BuildOpenStartInfo"/>'s arguments) is unit-testable.
    /// Failures notify rather than throw, via <see cref="Run"/>.</summary>
    /// <param name="runner">Process launcher (real or a test fake).</param>
    /// <param name="fullPath">Absolute path to open.</param>
    internal static void OpenTrustedWith(IProcessRunner runner, string fullPath) =>
        Run(
            Loc.Get("Shell_OpenFailed"),
            "open",
            fullPath,
            () => runner.Start(BuildOpenStartInfo(fullPath)));

    /// <summary>Identity-verified indexed-result core, exposed for boundary tests.</summary>
    /// <param name="verifier">Handle-bound path and identity verifier.</param>
    /// <param name="runner">Process launcher.</param>
    /// <param name="fullPath">MFT-sourced absolute path.</param>
    /// <param name="expectedFrn">Exact NTFS record-and-sequence identity.</param>
    internal static void OpenIndexedWith(
        IIndexedShellTargetVerifier verifier,
        IProcessRunner runner,
        string fullPath,
        ulong expectedFrn) =>
        Run(
            Loc.Get("Shell_OpenFailed"),
            "open-indexed",
            fullPath,
            () =>
            {
                using var lease = verifier.VerifyAndPin(fullPath, expectedFrn);
                runner.Start(BuildOpenStartInfo(fullPath));
            });

    /// <summary>Builds the explorer.exe invocation for "open". Kept internal and
    /// pure so the argument-safety contract is unit-testable without launching a
    /// process: <paramref name="fullPath"/> is attacker-influenced (the engine
    /// scans the raw MFT, which carries NTFS names the Win32 layer would reject —
    /// including the double quote), so it must travel as a single
    /// <see cref="ProcessStartInfo.ArgumentList"/> element, never concatenated
    /// into the <see cref="ProcessStartInfo.Arguments"/> command line where a quote
    /// could break out and inject explorer switches.</summary>
    /// <param name="fullPath">Absolute path to open, carried as a single argument.</param>
    /// <returns>The configured explorer.exe start info for the "open" launch.</returns>
    internal static ProcessStartInfo BuildOpenStartInfo(string fullPath)
    {
        var psi = new ProcessStartInfo { FileName = ExplorerPath, UseShellExecute = false };
        psi.ArgumentList.Add(fullPath);
        return psi;
    }

    /// <summary>
    /// Reveal an MFT-sourced result only while its exact identity and every
    /// path component remain pinned.
    /// </summary>
    /// <param name="fullPath">MFT-sourced absolute path.</param>
    /// <param name="expectedFrn">Exact NTFS record-and-sequence identity.</param>
    public static void RevealIndexed(string fullPath, ulong expectedFrn)
    {
        string failureMessage = Loc.Get("Shell_RevealFailed");
        var hooks = Volatile.Read(ref _hooks);
        hooks.StartStaThread(() => RevealOnSta(hooks, failureMessage, fullPath, expectedFrn));
    }

    /// <summary>STA-thread body: initialise COM, reveal, report any failure, and
    /// balance the COM init. Never lets an exception escape the thread (an
    /// unhandled one would tear down the process).</summary>
    /// <param name="hooks">Captured boundary set for the whole STA operation.</param>
    /// <param name="failureMessage">Pre-resolved headline for a failure notification.</param>
    /// <param name="fullPath">Absolute path to reveal and select.</param>
    /// <param name="expectedFrn">Exact indexed identity.</param>
    private static void RevealOnSta(
        ShellOpsHooks hooks,
        string failureMessage,
        string fullPath,
        ulong expectedFrn)
    {
        try
        {
            int coHr = hooks.CoInitialize();
            try
            {
                var failure = DoRevealIndexed(
                    hooks.Verifier,
                    hooks.RevealApi,
                    fullPath,
                    expectedFrn);
                if (failure is not null)
                {
                    ReportFailure(failureMessage, "reveal", fullPath, failure);
                }
            }
            finally
            {
                if (coHr >= 0)
                {
                    hooks.CoUninitialize();
                }
            }
        }
        catch (Exception ex)
        {
            ReportFailure(failureMessage, "reveal", fullPath, ex);
        }
    }

    /// <summary>Reveal-and-select orchestration, factored out so the HRESULT
    /// handling is unit-testable with a fake <see cref="IRevealApi"/>. Returns the
    /// failure to report, or <see langword="null"/> on success. Treats <em>any</em>
    /// non-<c>S_OK</c> HRESULT as failure — including non-negative ones like
    /// <c>S_FALSE</c> that <see cref="Marshal.ThrowExceptionForHR(int)"/> ignores;
    /// that silent-success gap is what shipped "reveal" broken. The PIDL is always
    /// freed once parsing succeeds.</summary>
    /// <param name="api">Shell calls (real or a test fake).</param>
    /// <param name="fullPath">Absolute path to reveal and select.</param>
    /// <returns>The failure exception, or <see langword="null"/> on success.</returns>
    private static Exception? DoReveal(IRevealApi api, string fullPath)
    {
        int hr = api.ParseDisplayName(fullPath, out var pidl);
        if (hr != 0)
        {
            return Marshal.GetExceptionForHR(hr) ?? RevealHrException(hr);
        }

        try
        {
            hr = api.OpenFolderAndSelectItems(pidl);
            return hr == 0 ? null : (Marshal.GetExceptionForHR(hr) ?? RevealHrException(hr));
        }
        finally
        {
            api.FreePidl(pidl);
        }
    }

    /// <summary>
    /// Identity-verified reveal core.  The path-component handles remain open
    /// until the shell has parsed the PIDL and accepted the reveal request.
    /// </summary>
    /// <param name="verifier">Handle-bound path and identity verifier.</param>
    /// <param name="api">Shell reveal API.</param>
    /// <param name="fullPath">MFT-sourced absolute path.</param>
    /// <param name="expectedFrn">Exact NTFS record-and-sequence identity.</param>
    /// <returns>The verification or shell failure, or null on success.</returns>
    internal static Exception? DoRevealIndexed(
        IIndexedShellTargetVerifier verifier,
        IRevealApi api,
        string fullPath,
        ulong expectedFrn)
    {
        try
        {
            using var lease = verifier.VerifyAndPin(fullPath, expectedFrn);
            return DoReveal(api, fullPath);
        }
        catch (Exception ex)
        {
            return ex;
        }
    }

    /// <summary>Exception for a non-negative HRESULT (e.g. <c>S_FALSE</c>) that
    /// <see cref="Marshal.GetExceptionForHR(int)"/> maps to <see langword="null"/>
    /// because its severity bit is clear — yet no window was shown.</summary>
    /// <param name="hr">The offending HRESULT.</param>
    /// <returns>A diagnostic exception carrying the HRESULT.</returns>
    private static InvalidOperationException RevealHrException(int hr) =>
        new($"reveal failed (SHOpenFolderAndSelectItems returned 0x{hr:X8})");

    /// <summary>True process restart, used only by the language switch so the
    /// <see cref="App"/> ctor re-applies <c>PrimaryLanguageOverride</c> and the
    /// window chrome is rebuilt. Goes through <see cref="AppInstance.Restart"/>
    /// (not <c>Process.Start</c> + <c>Application.Exit</c>) so the fresh instance
    /// wins single-instancing instead of redirecting back to this dying one
    /// (ADR-0036). Every other "restart" reason — service register or uninstall —
    /// is an in-process <see cref="AppReload"/> (App.SoftRestart). A
    /// failed restart notifies and leaves this instance running.</summary>
    public static void Relaunch() => RelaunchWith(Volatile.Read(ref _hooks).AppRestart);

    /// <summary>Relaunch core, parameterised over the restart step so it is
    /// unit-testable without actually terminating the process. A failure is
    /// funneled through <see cref="Run"/> (notify, don't crash).</summary>
    /// <param name="restart">Restart step (real or a test fake).</param>
    internal static void RelaunchWith(IAppRestart restart) =>
        Run(
            Loc.Get("Shell_RelaunchFailed"),
            "relaunch",
            "FindMyFiles",
            () => restart.Restart(string.Empty));

    /// <summary>Put <paramref name="text"/> on the clipboard. A failure is
    /// logged and surfaced as a warning notification (clipboard access can be
    /// transiently denied by other apps).</summary>
    /// <param name="text">The content to copy.</param>
    /// <param name="what">Short label for what is being copied, used in the
    /// failure log/notification (e.g. "path", "diagnostics").</param>
    public static void CopyText(string text, string what)
    {
        try
        {
            Volatile.Read(ref _hooks).CopyText(text);
        }
        catch (Exception ex)
        {
            FileLog.WarnEvent("shell", "clipboard copy failed", ex, ("operation", what));
            Notifier.Post(NotifySeverity.Warning, Loc.Get("Shell_ClipboardFailed"), ex.Message);
        }
    }

    private static void Run(string failureMessage, string operation, string path, Action action)
    {
        try
        {
            action();
        }
        catch (Exception ex)
        {
            ReportFailure(failureMessage, operation, path, ex);
        }
    }

    /// <summary>Log a shell-op failure and surface it as a warning notification
    /// (with a Win32-specific hint). Thread-safe — callable from the reveal STA
    /// thread as well as <see cref="Run"/> (<see cref="FileLog"/>/
    /// <see cref="Notifier"/> post from any thread).</summary>
    /// <param name="failureMessage">Localized headline.</param>
    /// <param name="operation">Stable operation name for privacy-safe logging.</param>
    /// <param name="path">Path the operation acted on (for the log + file name).</param>
    /// <param name="ex">The failure.</param>
    private static void ReportFailure(
        string failureMessage,
        string operation,
        string path,
        Exception ex)
    {
        FileLog.WarnEvent("shell", "shell operation failed", ex, ("operation", operation));
        var hint = Hint(ex, path);
        Notifier.Post(
            NotifySeverity.Warning,
            $"{failureMessage}: {Path.GetFileName(path)}",
            $"{ex.Message}({hint})");
    }

    /// <summary>Win32-error-specific hint — "access denied" must not read
    /// like "the file vanished" (the two have opposite remedies). Every failure
    /// resolves to some hint; the caller still tolerates an empty one and then
    /// shows the Win32 message alone.</summary>
    /// <param name="ex">The failure whose Win32 error code selects the hint.</param>
    /// <param name="path">The path acted on, which decides whether a
    /// "not found" is really a length failure.</param>
    /// <returns>The localized hint; unknown failures use the conservative
    /// "moved recently" fallback.</returns>
    internal static string Hint(Exception ex, string path) =>
        IsLengthFailure(ex, path)
            ? Loc.Get("Shell_HintPathTooLong")
            : (ex as System.ComponentModel.Win32Exception)?.NativeErrorCode switch
            {
                2 or 3 => Loc.Get("Shell_HintMoved"),            // FILE/PATH_NOT_FOUND
                5 => Loc.Get("Shell_HintAccessDenied"),          // ACCESS_DENIED
                1223 => Loc.Get("Shell_HintCancelled"),          // ERROR_CANCELLED
                _ => Loc.Get("Shell_HintMovedRecently"),
            };

    /// <summary>True when the length of <paramref name="path"/> — not a missing
    /// target — explains the failure. The engine indexes paths up to 32767 UTF-16
    /// units, and shell actions pin them through <c>CreateFileW</c>, which answers
    /// anything past <see cref="LegacyMaxPathChars"/> with ERROR_PATH_NOT_FOUND /
    /// ERROR_FILENAME_EXCED_RANGE whenever long-path support is not in effect
    /// (app.manifest declares <c>longPathAware</c>, but the machine policy
    /// <c>LongPathsEnabled</c> has to be on as well). Telling that user the file
    /// "may have been moved or deleted" sends them looking for a file that is
    /// exactly where they left it, so this case gets its own hint naming the
    /// machine setting that would fix it.</summary>
    /// <param name="ex">The failure to classify.</param>
    /// <param name="path">The path acted on.</param>
    /// <returns>True when the path length explains the failure.</returns>
    private static bool IsLengthFailure(Exception ex, string path) =>
        ex is PathTooLongException
        || (path.Length > LegacyMaxPathChars
            && (ex as System.ComponentModel.Win32Exception)?.NativeErrorCode
                is 2 or 3 or 206);

    private sealed class ActionOnDispose(Action action) : IDisposable
    {
        private Action? _action = action;

        public void Dispose() => Interlocked.Exchange(ref _action, null)?.Invoke();
    }
}
