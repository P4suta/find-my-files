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

    /// <summary>UI language: "auto" (follow the OS), "ja", "en", or "zh-Hans".
    /// Applied via PrimaryLanguageOverride in the App ctor; the gear menu's
    /// switcher persists it here and relaunches to take effect.</summary>
    public string Language { get; set; } = "auto";

    /// <summary>絞り込みモード (focused search, ADR-0019): rewrite queries in
    /// the UI with the two lists below before they reach the engine. On by
    /// default — the casual user wants a handful of hits, not 10,000; the
    /// toolbar toggle flips it per session and persists here.</summary>
    public bool FocusedSearch { get; set; } = true;

    /// <summary>Noise directories excluded in focused mode, each appended as
    /// a quoted <c>!path:"…"</c> term. Plain substring match against the full
    /// path (engine semantics) — no wildcards needed.</summary>
    public string[] FocusedExcludePaths { get; set; } =
    [
        @"\windows\",
        @"\program files",
        @"\programdata\",
        @"\$recycle.bin\",
        @"\node_modules\",
        @"\.git\",
        @"\__pycache__\",
    ];

    /// <summary>Extension whitelist applied in focused mode as a single
    /// OR-semantics <c>ext:a;b;…</c> term: documents, images, audio, video,
    /// archives and launchables — what a person actually goes looking for.</summary>
    public string[] FocusedExtensions { get; set; } =
    [
        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv",
        "jpg", "jpeg", "png", "gif", "webp", "svg", "heic",
        "mp3", "wav", "flac", "m4a",
        "mp4", "mkv", "mov", "avi",
        "zip", "7z", "rar",
        "exe", "msi", "lnk",
    ];

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
