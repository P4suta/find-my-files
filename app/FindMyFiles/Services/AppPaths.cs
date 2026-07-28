namespace FindMyFiles.Services;

/// <summary>
/// Canonical per-user app-state paths. UI settings and logs always live under
/// <c>%APPDATA%\find-my-files</c>; the executable directory is immutable
/// program material, not an alternate state root.
///
/// Test-seam builds may accept <c>--data-dir</c> so published UI automation can
/// isolate state. That parser is compiled out of stable artifacts.
/// </summary>
internal static class AppPaths
{
    private static readonly string Root = ResolveRoot();

    /// <summary>True only when a test-seam build accepted an isolated data root.</summary>
    public static bool IsTestOverride { get; private set; }

    /// <summary>Canonical per-user state root. Exposed so the explicit full
    /// uninstall flow can remove the whole UI-owned tree after closing the log.</summary>
    public static string RootDir => Root;

    /// <summary>User-scope settings at
    /// <c>%APPDATA%\find-my-files\settings.json</c>.</summary>
    public static string SettingsFile => Path.Combine(Root, "settings.json");

    /// <summary>User-scope logs at
    /// <c>%APPDATA%\find-my-files\logs</c>.</summary>
    public static string LogDir => Path.Combine(Root, "logs");

    private static string ResolveRoot()
    {
#if FMF_TEST_SEAMS
        const string prefix = "--data-dir=";
        var explicitDir = Environment.GetCommandLineArgs()
            .FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase));
        if (explicitDir is not null)
        {
            var value = explicitDir[prefix.Length..];
            if (!string.IsNullOrWhiteSpace(value))
            {
                IsTestOverride = true;
                return Path.GetFullPath(value);
            }
        }
#endif
        return Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "find-my-files");
    }
}
