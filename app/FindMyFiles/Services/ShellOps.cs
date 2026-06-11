using System.Diagnostics;
using Windows.ApplicationModel.DataTransfer;

namespace FindMyFiles.Services;

/// <summary>
/// Shell-facing operations, centralized so every failure path notifies the
/// user instead of crashing. Targets launch via explorer.exe to shed the
/// process's elevation (CLAUDE.md UI固定則).
/// </summary>
public static class ShellOps
{
    public static void Open(string fullPath)
    {
        Run("開けませんでした", fullPath, () =>
            Process.Start(new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"\"{fullPath}\"",
                UseShellExecute = false,
            }));
    }

    public static void Reveal(string fullPath)
    {
        Run("フォルダーを開けませんでした", fullPath, () =>
            Process.Start(new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"/select,\"{fullPath}\"",
                UseShellExecute = false,
            }));
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
            Notifier.Post(NotifySeverity.Warning, "クリップボードへのコピーに失敗しました", ex.Message);
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
            2 or 3 => "ファイルが移動・削除された可能性があります",     // FILE/PATH_NOT_FOUND
            5 => "アクセスが拒否されました — 権限を確認してください",   // ACCESS_DENIED
            _ => "ファイルが移動・削除された直後の可能性があります",
        };
}
