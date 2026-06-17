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

    /// <summary>Absolute paths of the root folders to index in non-elevated
    /// scope mode (ADR-0024). Empty = unconfigured, and the setup screen mainly
    /// pushes the admin path (all drives, fastest). One or more entries resolve
    /// to <c>EngineChoice.WalkInProc</c> on the next launch, so folder-walk
    /// search works on a corporate PC where neither the service nor admin rights
    /// are available.</summary>
    public string[] ScopeRoots { get; set; } = [];

    /// <summary>Absolute subfolder paths pruned from the scope walk (ADR-0025).
    /// Each must lie under one of <see cref="ScopeRoots"/>; the walk skips the
    /// matching subtree so it is never indexed. Empty = index everything under
    /// the roots.</summary>
    public string[] ScopeExcludes { get; set; } = [];

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        WriteIndented = true,
    };

    /// <summary>Absolute path to the user-scope settings file. Portable by
    /// default (<c>&lt;exe&gt;\data\settings.json</c>); falls back to
    /// <c>%APPDATA%\find-my-files\settings.json</c> when the app folder is
    /// read-only. See <see cref="AppPaths"/>.</summary>
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

    /// <summary>Persist the current settings to <see cref="SettingsPath"/>
    /// (snake_case JSON, indented). Best-effort: a write failure is logged, not
    /// thrown.</summary>
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
