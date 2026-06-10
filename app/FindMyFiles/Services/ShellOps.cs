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
            // The most common cause: the file vanished/renamed after indexing.
            Notifier.Post(
                NotifySeverity.Warning,
                $"{failureMessage}: {Path.GetFileName(path)}",
                $"{ex.Message}(ファイルが移動・削除された直後の可能性があります)");
        }
    }
}
