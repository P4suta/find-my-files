namespace FindMyFiles.Services;

/// <summary>
/// Resolves where the app keeps its state — <b>portable by default</b>
/// (ADR-0024). On first access it picks one data root, once, in this order:
/// <list type="number">
/// <item><c>--data-dir=&lt;path&gt;</c> when given (tests, scratch, power users);</item>
/// <item><c>&lt;exe&gt;\data</c> when it can be created and written — the default,
/// so a copied/unzipped build keeps everything in its own folder and touches
/// neither the user profile nor the registry ("just drop it / just delete it");</item>
/// <item>the per-user profile (<c>%APPDATA%</c> / <c>%LOCALAPPDATA%</c>) only when the
/// app folder is read-only (e.g. installed under Program Files).</item>
/// </list>
/// The machine-scope service index (<c>%ProgramData%</c>) and the admin in-proc
/// index are intentionally <i>not</i> redirected — that path is an install by
/// nature, the opposite of portable.
/// <para>Resolution is silent: it must not call <see cref="FileLog"/>, because
/// FileLog's directory comes from here (a cycle otherwise).</para>
/// </summary>
public static class AppPaths
{
    private static readonly Lazy<string?> PortableRootLazy = new(ResolvePortableRoot);

    /// <summary>True when state lives next to the exe (or <c>--data-dir</c>),
    /// not in the user profile.</summary>
    public static bool IsPortable => PortableRootLazy.Value is not null;

    /// <summary>The portable data root, or <see langword="null"/> when falling
    /// back to the per-user profile. Surfaced for the setup screen / diagnostics
    /// ("running portable from …").</summary>
    public static string? PortableRoot => PortableRootLazy.Value;

    /// <summary>User-scope settings file — portable <c>&lt;data&gt;\settings.json</c>,
    /// else <c>%APPDATA%\find-my-files\settings.json</c>.</summary>
    public static string SettingsFile => PortableRoot is { } r
        ? Path.Combine(r, "settings.json")
        : Path.Combine(AppData, "find-my-files", "settings.json");

    /// <summary>App + scope-engine log directory — portable <c>&lt;data&gt;\logs</c>,
    /// else <c>%APPDATA%\find-my-files\logs</c>.</summary>
    public static string LogDir => PortableRoot is { } r
        ? Path.Combine(r, "logs")
        : Path.Combine(AppData, "find-my-files", "logs");

    /// <summary>Scope-mode (ADR-0024) index directory — portable
    /// <c>&lt;data&gt;\index</c>, else <c>%LOCALAPPDATA%\find-my-files\index</c>.</summary>
    public static string ScopeIndexDir => PortableRoot is { } r
        ? Path.Combine(r, "index")
        : Path.Combine(LocalAppData, "find-my-files", "index");

    private static string AppData =>
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);

    private static string LocalAppData =>
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);

    private static string? ResolvePortableRoot()
    {
        // Packaged (.msix) build (ADR-0028 R4): the install dir under WindowsApps
        // is read-only, and a packaged process's %APPDATA%/%LOCALAPPDATA% is
        // copy-on-write redirected to a per-package store — so portable
        // <exe>\data is neither writable nor appropriate. Force the profile path
        // (IsPortable => false). A zip user who was on the profile-fallback path
        // migrates transparently: the redirected %APPDATA% reads fall through to
        // the real settings.json until the first save copies it into the store.
        if (PackageIdentity.IsPackaged)
        {
            return null;
        }

        var explicitDir = DataDirArg();
        if (!string.IsNullOrWhiteSpace(explicitDir) && TryEnsureWritable(explicitDir))
        {
            return Path.GetFullPath(explicitDir);
        }

        var exeData = Path.Combine(AppContext.BaseDirectory, "data");
        return TryEnsureWritable(exeData) ? exeData : null;
    }

    private static string? DataDirArg()
    {
        const string prefix = "--data-dir=";
        foreach (var a in Environment.GetCommandLineArgs())
        {
            if (a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            {
                return a[prefix.Length..];
            }
        }

        return null;
    }

    /// <summary>Create <paramref name="dir"/> and confirm a file can be written
    /// in it (the probe catches read-only Program Files installs where the
    /// directory create "succeeds" via virtualization but writes fail).</summary>
    private static bool TryEnsureWritable(string dir)
    {
        try
        {
            Directory.CreateDirectory(dir);
            var probe = Path.Combine(dir, ".write-probe");
            File.WriteAllText(probe, string.Empty);
            File.Delete(probe);
            return true;
        }
        catch
        {
            return false;
        }
    }
}
