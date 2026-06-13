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
    public static void Open(string fullPath) =>
        Run(Loc.Get("Shell_OpenFailed"), fullPath, () => Process.Start(BuildOpenStartInfo(fullPath)));

    /// <summary>Builds the explorer.exe invocation for "open". Kept internal and
    /// pure so the argument-safety contract is unit-testable without launching a
    /// process: <paramref name="fullPath"/> is attacker-influenced (the engine
    /// scans the raw MFT, which carries NTFS names the Win32 layer would reject —
    /// including the double quote), so it must travel as a single
    /// <see cref="ProcessStartInfo.ArgumentList"/> element, never concatenated
    /// into the <see cref="ProcessStartInfo.Arguments"/> command line where a quote
    /// could break out and inject explorer switches.</summary>
    internal static ProcessStartInfo BuildOpenStartInfo(string fullPath)
    {
        var psi = new ProcessStartInfo { FileName = ExplorerPath, UseShellExecute = false };
        psi.ArgumentList.Add(fullPath);
        return psi;
    }

    /// <summary>Reveal a file in Explorer with it selected, via the shell API
    /// (<c>SHParseDisplayName</c> + <c>SHOpenFolderAndSelectItems</c>) so a
    /// quote in an MFT-sourced name cannot inject explorer switches. Failures
    /// notify rather than throw.</summary>
    /// <param name="fullPath">Absolute path to reveal and select.</param>
    public static void Reveal(string fullPath) =>
        Run(Loc.Get("Shell_RevealFailed"), fullPath, () =>
        {
            // Reveal-and-select through the shell API, not `explorer.exe /select,<path>`:
            // explorer's switch parser needs a literal quoted path and honours no
            // escaping, so ArgumentList cannot express it and a '"' in a name (the MFT
            // scan surfaces raw NTFS names) could break out of the quotes and inject
            // explorer switches. SHParseDisplayName takes the path as data, never a
            // command line — injection is impossible by construction, and a missing
            // path simply yields a failing HRESULT that Run() reports.
            Marshal.ThrowExceptionForHR(
                SHParseDisplayName(fullPath, IntPtr.Zero, out var pidl, 0, out _));
            try
            {
                Marshal.ThrowExceptionForHR(SHOpenFolderAndSelectItems(pidl, 0, null, 0));
            }
            finally
            {
                Marshal.FreeCoTaskMem(pidl);
            }
        });

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
            FileLog.Warn("shell", $"shell op failed for {path}", ex);
            Notifier.Post(
                NotifySeverity.Warning,
                $"{failureMessage}: {Path.GetFileName(path)}",
                $"{ex.Message}({Hint(ex)})");
        }
    }

    /// <summary>Win32-error-specific hint — "access denied" must not read
    /// like "the file vanished" (the two have opposite remedies).</summary>
    private static string Hint(Exception ex) =>
        (ex as System.ComponentModel.Win32Exception)?.NativeErrorCode switch
        {
            2 or 3 => Loc.Get("Shell_HintMoved"),            // FILE/PATH_NOT_FOUND
            5 => Loc.Get("Shell_HintAccessDenied"),          // ACCESS_DENIED
            1223 => Loc.Get("Shell_HintCancelled"),          // ERROR_CANCELLED
            _ => Loc.Get("Shell_HintMovedRecently"),
        };

    // Reveal selects an item via the shell instead of an explorer.exe command line
    // (see Reveal). Pinned to System32 — the same binary-planting defence the
    // ExplorerPath constant applies to explorer.exe.
    [LibraryImport("shell32.dll", StringMarshalling = StringMarshalling.Utf16)]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int SHParseDisplayName(
        string name, IntPtr bindingContext, out IntPtr pidl, uint sfgaoIn, out uint psfgaoOut);

    [LibraryImport("shell32.dll")]
    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    private static partial int SHOpenFolderAndSelectItems(
        IntPtr pidlFolder, uint cidl, IntPtr[]? apidl, uint dwFlags);
}
