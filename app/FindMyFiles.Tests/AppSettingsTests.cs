using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class AppSettingsTests
{
    [Fact]
    public void Defaults_FocusedSearchIsOn_WithNonEmptyLists()
    {
        var s = new AppSettings();
        Assert.True(s.FocusedSearch);
        Assert.Contains(@"\windows\", s.FocusedExcludePaths);
        Assert.Contains("pdf", s.FocusedExtensions);
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
                FocusedExcludePaths = [@"\scratch\"],
                FocusedExtensions = ["pdf", "md"],
            };
            s.SaveTo(path);

            var loaded = AppSettings.LoadFrom(path);
            Assert.False(loaded.FocusedSearch);
            Assert.Equal([@"\scratch\"], loaded.FocusedExcludePaths);
            Assert.Equal(["pdf", "md"], loaded.FocusedExtensions);

            // The wire names are stable snake_case — what users hand-edit.
            var json = File.ReadAllText(path);
            Assert.Contains("\"focused_search\"", json);
            Assert.Contains("\"focused_exclude_paths\"", json);
            Assert.Contains("\"focused_extensions\"", json);
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
            new AppSettings { RegexMode = true, RegexScope = "path" }.SaveTo(path);

            var loaded = AppSettings.LoadFrom(path);
            Assert.True(loaded.RegexMode);
            Assert.Equal("path", loaded.RegexScope);

            var json = File.ReadAllText(path);
            Assert.Contains("\"regex_mode\"", json);
            Assert.Contains("\"regex_scope\"", json);
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
        // A pre-feature settings.json (engine only) keeps working and gains
        // the focused defaults — the corruption-tolerance pattern's cousin.
        var dir = Path.Combine(Path.GetTempPath(), "fmf-settings-" + Guid.NewGuid().ToString("N"));
        var path = Path.Combine(dir, "settings.json");
        try
        {
            Directory.CreateDirectory(dir);
            File.WriteAllText(path, "{ \"engine\": \"pipe\" }");

            var loaded = AppSettings.LoadFrom(path);
            Assert.Equal("pipe", loaded.Engine);
            Assert.True(loaded.FocusedSearch);
            Assert.NotEmpty(loaded.FocusedExcludePaths);
            Assert.NotEmpty(loaded.FocusedExtensions);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }
}
