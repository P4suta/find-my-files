using System.Diagnostics;
using Microsoft.UI.Xaml;
using Windows.ApplicationModel.DataTransfer;

namespace FindMyFiles.Services;

/// <summary>
/// Shell-facing operations, centralized so every failure path notifies the
/// user instead of crashing. Targets launch via explorer.exe to shed the
/// process's elevation (CLAUDE.md UI固定則).
/// </summary>
public static class ShellOps
{
    /// <summary>Full path to explorer.exe (<c>%WINDIR%\explorer.exe</c>).
    /// Launching by bare name under <c>UseShellExecute=false</c> lets
    /// CreateProcess search the current directory first — a binary-planting
    /// vector. Pin it to the Windows directory.</summary>
    private static readonly string ExplorerPath =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe");

    public static void Open(string fullPath)
    {
        Run(Loc.Get("Shell_OpenFailed"), fullPath, () =>
            Process.Start(new ProcessStartInfo
            {
                FileName = ExplorerPath,
                Arguments = $"\"{fullPath}\"",
                UseShellExecute = false,
            }));
    }

    public static void Reveal(string fullPath)
    {
        Run(Loc.Get("Shell_RevealFailed"), fullPath, () =>
            Process.Start(new ProcessStartInfo
            {
                FileName = ExplorerPath,
                Arguments = $"/select,\"{fullPath}\"",
                UseShellExecute = false,
            }));
    }

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
}
