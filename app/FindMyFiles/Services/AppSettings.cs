using System.Text.Json;

namespace FindMyFiles.Services;

/// <summary>
/// User-scope settings at %APPDATA%\find-my-files\settings.json — UI-owned,
/// deliberately separate from the machine-scope service.json the service
/// owns. A corrupt file degrades to defaults: warn, quarantine as .bad, and
/// the next save starts clean.
/// </summary>
public sealed class AppSettings
{
    /// <summary>Engine transport: "auto" (pipe probe → FFI fallback),
    /// "pipe", or "inproc". CLI flags override this.</summary>
    public string Engine { get; set; } = "auto";

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        WriteIndented = true,
    };

    public static string SettingsPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "find-my-files", "settings.json");

    public static AppSettings Load() => LoadFrom(SettingsPath);

    internal static AppSettings LoadFrom(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return new AppSettings();
            }
            return JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(path), JsonOpts)
                ?? new AppSettings();
        }
        catch (Exception ex)
        {
            FileLog.Warn("settings", $"unreadable settings.json — using defaults ({path})", ex);
            Quarantine(path);
            return new AppSettings();
        }
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

    public void Save() => SaveTo(SettingsPath);

    internal void SaveTo(string path)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, JsonSerializer.Serialize(this, JsonOpts));
        }
        catch (Exception ex)
        {
            FileLog.Warn("settings", "failed to save settings.json", ex);
        }
    }
}
