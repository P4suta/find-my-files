using System.Text.Json;

namespace FindMyFiles.Services;

/// <summary>
/// User-scope settings at %APPDATA%\find-my-files\settings.json — UI-owned,
/// deliberately separate from the machine-scope service.json the service
/// owns. A corrupt file degrades to defaults: warn, quarantine as .bad, and
/// the next save starts clean.
/// </summary>
internal sealed class AppSettings
{
    private const int MaxSettingsBytes = 16 * 1024;

    /// <summary>UI language: "auto" (follow the OS), "ja", "en", or "zh-Hans".
    /// Applied via PrimaryLanguageOverride in the App ctor; the gear menu's
    /// switcher persists it here and relaunches to take effect.</summary>
    public string Language { get; set; } = "auto";

    /// <summary>Focused-search mode (ADR-0019): rewrite queries in
    /// the UI with the two lists below before they reach the engine. On by
    /// default — the casual user wants a handful of hits, not 10,000; the
    /// toolbar toggle flips it per session and persists here.</summary>
    public bool FocusedSearch { get; set; } = true;

    /// <summary>Regex mode (ADR-0023): treat the whole query as one regex.
    /// Off by default; the gear-menu toggle flips it and persists here.</summary>
    public bool RegexMode { get; set; }

    /// <summary>Which haystack the whole-query regex matches — "name" or
    /// "path". Kept independent of <see cref="RegexMode"/> so the choice
    /// survives toggling regex off and back on. Unknown values fall back to
    /// "name".</summary>
    public string RegexScope { get; set; } = "name";

    /// <summary>Tray-resident mode (ADR-0030): when on, the close (×) button
    /// hides the window to the system tray instead of exiting and the process
    /// stays alive with its engine connection hot, so re-opening is instant and
    /// the first search pays no cold start. Off by default — close exits and the
    /// service returns to its on-demand idle-stop (ADR-0027). The gear-menu
    /// toggle flips it and persists here.</summary>
    public bool CloseToTray { get; set; }

    /// <summary>Absolute path to the canonical user-scope settings file at
    /// <c>%APPDATA%\find-my-files\settings.json</c>.</summary>
    public static string SettingsPath => AppPaths.SettingsFile;

    /// <summary>Load settings from <see cref="SettingsPath"/>, falling back to
    /// defaults (and quarantining the file) if it is missing or corrupt.</summary>
    /// <returns>The loaded settings, or a fresh default instance.</returns>
    public static AppSettings Load() => LoadFrom(SettingsPath);

    internal static AppSettings LoadFrom(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return new AppSettings();
            }

            var settings =
                JsonSerializer.Deserialize(
                    ReadBounded(path),
                    AppSettingsJsonContext.Default.AppSettings)
                ?? new AppSettings();
            settings.Normalize();
            return settings;
        }
        catch (Exception ex)
        {
            FileLog.Warn("settings", "unreadable settings.json — using defaults", ex);
            Quarantine(path);
            return new AppSettings();
        }
    }

    private static byte[] ReadBounded(string path)
    {
        using var stream = new FileStream(
            path,
            FileMode.Open,
            FileAccess.Read,
            FileShare.Read,
            bufferSize: 4096,
            FileOptions.SequentialScan);
        if (stream.Length > MaxSettingsBytes)
        {
            throw new InvalidDataException(
                $"settings.json exceeds {MaxSettingsBytes} bytes");
        }

        var bytes = new byte[checked((int)stream.Length)];
        stream.ReadExactly(bytes);
        if (stream.ReadByte() != -1)
        {
            throw new InvalidDataException(
                $"settings.json exceeds {MaxSettingsBytes} bytes");
        }

        return bytes;
    }

    /// <summary>
    /// JSON can assign null to non-nullable reference properties and older or
    /// hand-edited files can carry unsupported scalar values. Normalize every
    /// persisted value before it reaches WinRT or query construction.
    /// </summary>
    private void Normalize()
    {
        Language = Language switch
        {
            "auto" or "ja" or "en" or "zh-Hans" => Language,
            _ => "auto",
        };
        RegexScope = string.Equals(RegexScope, "path", StringComparison.Ordinal)
            ? "path"
            : "name";
    }

    private static void Quarantine(string path)
    {
        try
        {
            File.Move(path, path + ".bad", overwrite: true);
        }
        catch (Exception ex)
        {
            FileLog.Warn("settings", "could not quarantine corrupt settings.json", ex);
        }
    }

    /// <summary>Persist the current settings to <see cref="SettingsPath"/>
    /// (snake_case JSON, indented). A write failure is logged and returned to
    /// the caller so UI state cannot claim an unpersisted change succeeded.</summary>
    /// <returns>True only after the atomic replacement completed.</returns>
    public bool Save() => SaveTo(SettingsPath);

    /// <summary>Path-parameterized persistence core.</summary>
    /// <param name="path">Absolute settings file path to replace atomically.</param>
    /// <returns>True only after the atomic replacement completed.</returns>
    internal bool SaveTo(string path)
    {
        string? temp = null;
        try
        {
            Normalize();
            var directory = Path.GetDirectoryName(path)
                ?? throw new ArgumentException("settings path has no parent directory", nameof(path));
            Directory.CreateDirectory(directory);

            temp = Path.Combine(
                directory,
                $".{Path.GetFileName(path)}.{Environment.ProcessId}.{Guid.NewGuid():N}.tmp");
            var json = JsonSerializer.Serialize(this, AppSettingsJsonContext.Default.AppSettings);
            using (var stream = new FileStream(
                temp,
                FileMode.CreateNew,
                FileAccess.Write,
                FileShare.None,
                bufferSize: 4096,
                FileOptions.WriteThrough))
            using (var writer = new StreamWriter(stream, new System.Text.UTF8Encoding(false)))
            {
                writer.Write(json);
                writer.Flush();
                stream.Flush(flushToDisk: true);
            }

            File.Move(temp, path, overwrite: true);
            temp = null;
            return true;
        }
        catch (Exception ex)
        {
            FileLog.Warn("settings", "failed to save settings.json", ex);
            return false;
        }
        finally
        {
            if (temp is not null)
            {
                try
                {
                    File.Delete(temp);
                }
                catch (Exception ex)
                {
                    FileLog.Warn("settings", "failed to remove temporary settings file", ex);
                }
            }
        }
    }
}
