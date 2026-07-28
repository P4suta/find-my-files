using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class AppSettingsTests
{
    [Fact]
    public void Defaults_FocusedSearchIsOn()
    {
        var s = new AppSettings();
        Assert.True(s.FocusedSearch);
    }

    [Fact]
    public void FocusedSettings_RoundTripThroughDisk()
    {
        var dir = Path.Combine(Path.GetTempPath(), "fmf-settings-" + Guid.NewGuid().ToString("N"));
        var path = Path.Combine(dir, "settings.json");
        try
        {
            var s = new AppSettings
            {
                FocusedSearch = false,
            };
            Assert.True(s.SaveTo(path));

            var loaded = AppSettings.LoadFrom(path);
            Assert.False(loaded.FocusedSearch);

            // Only user-facing settings are persisted. Focus policy is
            // code-owned rather than a hidden hand-edited schema.
            var json = File.ReadAllText(path);
            Assert.Contains("\"focused_search\"", json, StringComparison.Ordinal);
            Assert.DoesNotContain("\"engine\"", json, StringComparison.Ordinal);
            Assert.DoesNotContain("\"focused_exclude_paths\"", json, StringComparison.Ordinal);
            Assert.DoesNotContain("\"focused_extensions\"", json, StringComparison.Ordinal);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }

    [Fact]
    public void RegexSettings_DefaultOffNameScope_AndRoundTrip()
    {
        var fresh = new AppSettings();
        Assert.False(fresh.RegexMode);
        Assert.Equal("name", fresh.RegexScope);

        var dir = Path.Combine(Path.GetTempPath(), "fmf-settings-" + Guid.NewGuid().ToString("N"));
        var path = Path.Combine(dir, "settings.json");
        try
        {
            Assert.True(new AppSettings { RegexMode = true, RegexScope = "path" }.SaveTo(path));

            var loaded = AppSettings.LoadFrom(path);
            Assert.True(loaded.RegexMode);
            Assert.Equal("path", loaded.RegexScope);

            var json = File.ReadAllText(path);
            Assert.Contains("\"regex_mode\"", json, StringComparison.Ordinal);
            Assert.Contains("\"regex_scope\"", json, StringComparison.Ordinal);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }

    [Fact]
    public void MissingKeys_FallBackToDefaults()
    {
        // Legacy hidden keys are ignored and visible settings retain defaults.
        var dir = Path.Combine(Path.GetTempPath(), "fmf-settings-" + Guid.NewGuid().ToString("N"));
        var path = Path.Combine(dir, "settings.json");
        try
        {
            Directory.CreateDirectory(dir);
            File.WriteAllText(path, "{ \"engine\": \"pipe\" }");

            var loaded = AppSettings.LoadFrom(path);
            Assert.True(loaded.FocusedSearch);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }

    [Fact]
    public void CloseToTray_DefaultOff_AndRoundTrip()
    {
        // Default off keeps the unchanged ADR-0027 on-demand behaviour for
        // users who never opt in (ADR-0030).
        Assert.False(new AppSettings().CloseToTray);

        var dir = Path.Combine(Path.GetTempPath(), "fmf-settings-" + Guid.NewGuid().ToString("N"));
        var path = Path.Combine(dir, "settings.json");
        try
        {
            Assert.True(new AppSettings { CloseToTray = true }.SaveTo(path));

            var loaded = AppSettings.LoadFrom(path);
            Assert.True(loaded.CloseToTray);

            // Stable snake_case wire name — what users hand-edit.
            var json = File.ReadAllText(path);
            Assert.Contains("\"close_to_tray\"", json, StringComparison.Ordinal);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }

    [Theory]
    [InlineData("{\"language\":null,\"regex_scope\":null}", "auto", "name")]
    [InlineData("{\"language\":\"bogus\",\"regex_scope\":\"bogus\"}", "auto", "name")]
    [InlineData("{\"language\":\"zh-Hans\",\"regex_scope\":\"path\"}", "zh-Hans", "path")]
    public void Load_normalizes_null_and_unknown_scalar_values(
        string json,
        string expectedLanguage,
        string expectedScope)
    {
        var dir = Directory.CreateTempSubdirectory("fmf-settings-");
        try
        {
            var path = Path.Combine(dir.FullName, "settings.json");
            File.WriteAllText(path, json);

            var loaded = AppSettings.LoadFrom(path);

            Assert.Equal(expectedLanguage, loaded.Language);
            Assert.Equal(expectedScope, loaded.RegexScope);
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    [Fact]
    public void Save_atomically_replaces_existing_file_and_leaves_no_temp()
    {
        var dir = Directory.CreateTempSubdirectory("fmf-settings-");
        try
        {
            var path = Path.Combine(dir.FullName, "settings.json");
            File.WriteAllText(path, "{\"language\":\"ja\"}");

            Assert.True(new AppSettings { Language = "en", RegexMode = true }.SaveTo(path));

            var loaded = AppSettings.LoadFrom(path);
            Assert.Equal("en", loaded.Language);
            Assert.True(loaded.RegexMode);
            Assert.Empty(Directory.EnumerateFiles(dir.FullName, "*.tmp"));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }
}
