namespace FindMyFiles.Services;

/// <summary>Deletes the UI-owned per-user state for the explicit full-uninstall
/// action. The app log is closed first because Serilog owns <c>app.log</c> with
/// delete sharing disabled. On failure logging is reopened so the still-running
/// app does not silently lose diagnostics.</summary>
internal static class AppDataPurger
{
    /// <summary>Production full-purge entry point.</summary>
    /// <returns>True only when the canonical AppData tree no longer exists.</returns>
    public static bool TryPurge()
    {
        if (!AppPaths.IsTestOverride && !IsCanonicalRoot(AppPaths.RootDir))
        {
            FileLog.Warn("service-ui", "refusing to purge an unexpected per-user data root");
            return false;
        }

        var ok = TryPurge(AppPaths.RootDir, LogSetup.Shutdown);
        if (!ok)
        {
            LogSetup.Init();
            FileLog.Warn("service-ui", "could not remove per-user app data");
        }

        return ok;
    }

    private static bool IsCanonicalRoot(string root)
    {
        var expected = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "find-my-files");
        return string.Equals(
            Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar),
            Path.GetFullPath(expected).TrimEnd(Path.DirectorySeparatorChar),
            StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Path-parameterized core for deterministic tests.</summary>
    /// <param name="root">Per-user state root to remove.</param>
    /// <param name="closeLog">Closes the owner of files below <paramref name="root"/>.</param>
    /// <returns>True only when the root is absent after the operation.</returns>
    internal static bool TryPurge(string root, Action closeLog)
    {
        try
        {
            closeLog();
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }

            return !Directory.Exists(root);
        }
        catch
        {
            return false;
        }
    }
}
