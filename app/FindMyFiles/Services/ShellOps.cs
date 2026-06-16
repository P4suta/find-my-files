using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;
using Windows.ApplicationModel.DataTransfer;

namespace FindMyFiles.Services;

/// <summary>
/// Shell-facing operations, centralized so every failure path notifies the
/// user instead of crashing. Targets launch via explorer.exe to shed the
/// process's elevation (CLAUDE.md UI固定則).
/// </summary>
public static partial class ShellOps
{
    /// <summary><c>COINIT_APARTMENTTHREADED</c> — reveal runs on a dedicated STA.</summary>
    private const uint COINITAPARTMENTTHREADED = 0x2;

    /// <summary>Full path to explorer.exe (<c>%WINDIR%\explorer.exe</c>).
    /// Launching by bare name under <c>UseShellExecute=false</c> lets
    /// CreateProcess search the current directory first — a binary-planting
    /// vector. Pin it to the Windows directory.</summary>
    private static readonly string ExplorerPath =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe");

    /// <summary>Open a file or folder with its default handler via
    /// explorer.exe, shedding the app's elevation. Failures notify the user
    /// (with a Win32-specific hint) rather than throwing.</summary>
    /// <param name="fullPath">Absolute path to open; treated as data, never as
    /// a command line (see <see cref="BuildOpenStartInfo"/>).</param>
    public static void Open(string fullPath) => OpenWith(RealProcessRunner.Instance, fullPath);

    /// <summary>"Open" core, parameterised over the process runner so the launch
    /// (not just <see cref="BuildOpenStartInfo"/>'s arguments) is unit-testable.
    /// Failures notify rather than throw, via <see cref="Run"/>.</summary>
    /// <param name="runner">Process launcher (real or a test fake).</param>
    /// <param name="fullPath">Absolute path to open.</param>
    internal static void OpenWith(IProcessRunner runner, string fullPath) =>
        Run(Loc.Get("Shell_OpenFailed"), fullPath, () => runner.Start(BuildOpenStartInfo(fullPath)));

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

    /// <summary>Reveal a file in Explorer with it selected, via the shell API
    /// (<c>SHParseDisplayName</c> + <c>SHOpenFolderAndSelectItems</c>) — never
    /// <c>explorer.exe /select,&lt;path&gt;</c>, whose switch parser needs a literal
    /// quoted path it does not escape, so a '"' in an MFT-sourced name could inject
    /// switches. Runs on a dedicated STA thread with COM initialised: on the WinUI
    /// UI thread (an ASTA) <c>SHOpenFolderAndSelectItems</c> returns <c>S_OK</c> but
    /// opens nothing. Failures notify off-thread (<see cref="Notifier"/>/
    /// <see cref="FileLog"/> are thread-safe; the ViewModel marshals the InfoBar to
    /// the UI).</summary>
    /// <param name="fullPath">Absolute path to reveal and select.</param>
    public static void Reveal(string fullPath)
    {
        // Resolve the localized message on the UI thread, then hand off.
        string failureMessage = Loc.Get("Shell_RevealFailed");
        var thread = new Thread(() => RevealOnSta(failureMessage, fullPath)) { IsBackground = true };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
    }

    /// <summary>STA-thread body: initialise COM, reveal, report any failure, and
    /// balance the COM init. Never lets an exception escape the thread (an
    /// unhandled one would tear down the process).</summary>
    /// <param name="failureMessage">Pre-resolved headline for a failure notification.</param>
    /// <param name="fullPath">Absolute path to reveal and select.</param>
    private static void RevealOnSta(string failureMessage, string fullPath)
    {
        int coHr = CoInitializeEx(IntPtr.Zero, COINITAPARTMENTTHREADED);
        try
        {
            if (DoReveal(RealRevealApi.Instance, fullPath) is { } failure)
            {
                ReportFailure(failureMessage, fullPath, failure);
            }
        }
        catch (Exception ex)
        {
            ReportFailure(failureMessage, fullPath, ex);
        }
        finally
        {
            if (coHr >= 0)
            {
                CoUninitialize();
            }
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
    internal static Exception? DoReveal(IRevealApi api, string fullPath)
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

    /// <summary>Exception for a non-negative HRESULT (e.g. <c>S_FALSE</c>) that
    /// <see cref="Marshal.GetExceptionForHR(int)"/> maps to <see langword="null"/>
    /// because its severity bit is clear — yet no window was shown.</summary>
    /// <param name="hr">The offending HRESULT.</param>
    /// <returns>A diagnostic exception carrying the HRESULT.</returns>
    private static InvalidOperationException RevealHrException(int hr) =>
        new($"reveal failed (SHOpenFolderAndSelectItems returned 0x{hr:X8})");

    /// <summary>Relaunch this app (unelevated — no runas) and exit, used right
    /// after an in-app service registration so the fresh instance picks up the
    /// now-running service over the pipe (the engine transport is chosen once,
    /// at startup). Strictly user-initiated (the「アプリを再起動」button). A
    /// failed launch notifies and leaves the current instance running.</summary>
    public static void Relaunch()
    {
        Run(Loc.Get("Shell_RelaunchFailed"), "FindMyFiles", () =>
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = Environment.ProcessPath!,
                UseShellExecute = true,
            });

            // Only reached when the new instance actually launched.
            Application.Current.Exit();
        });
    }

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
            var pkg = new DataPackage();
            pkg.SetText(text);
            Clipboard.SetContent(pkg);
        }
        catch (Exception ex)
        {
            FileLog.Warn("shell", $"clipboard copy failed ({what})", ex);
            Notifier.Post(NotifySeverity.Warning, Loc.Get("Shell_ClipboardFailed"), ex.Message);
        }
    }

    private static void Run(string failureMessage, string path, Action action)
    {
        try
        {
            action();
        }
        catch (Exception ex)
        {
            ReportFailure(failureMessage, path, ex);
        }
    }

    /// <summary>Log a shell-op failure and surface it as a warning notification
    /// (with a Win32-specific hint). Thread-safe — callable from the reveal STA
    /// thread as well as <see cref="Run"/> (<see cref="FileLog"/>/
    /// <see cref="Notifier"/> post from any thread).</summary>
    /// <param name="failureMessage">Localized headline.</param>
    /// <param name="path">Path the operation acted on (for the log + file name).</param>
    /// <param name="ex">The failure.</param>
    private static void ReportFailure(string failureMessage, string path, Exception ex)
    {
        FileLog.Warn("shell", $"shell op failed for {path}", ex);
        Notifier.Post(
            NotifySeverity.Warning,
            $"{failureMessage}: {Path.GetFileName(path)}",
            $"{ex.Message}({Hint(ex)})");
    }

    /// <summary>Win32-error-specific hint — "access denied" must not read
    /// like "the file vanished" (the two have opposite remedies).</summary>
    /// <param name="ex">The failure whose Win32 error code selects the hint.</param>
    private static string Hint(Exception ex) =>
        (ex as System.ComponentModel.Win32Exception)?.NativeErrorCode switch
        {
            2 or 3 => Loc.Get("Shell_HintMoved"),            // FILE/PATH_NOT_FOUND
            5 => Loc.Get("Shell_HintAccessDenied"),          // ACCESS_DENIED
            1223 => Loc.Get("Shell_HintCancelled"),          // ERROR_CANCELLED
            _ => Loc.Get("Shell_HintMovedRecently"),
        };

    // COM init for the reveal STA thread (SHOpenFolderAndSelectItems needs an
    // initialised STA). Pinned to System32 like the other shell imports.
    [LibraryImport("ole32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int CoInitializeEx(IntPtr reserved, uint coInit);

    [LibraryImport("ole32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial void CoUninitialize();
}
