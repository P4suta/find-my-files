using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Error-path tests for <see cref="AppSettings"/> via the
/// path-parameterised <c>LoadFrom</c>/<c>SaveTo</c> internals — missing and
/// corrupt files must degrade to defaults (and quarantine), and a roundtrip
/// must preserve non-default values.</summary>
public sealed class AppSettingsErrorTests
{
    private static string TempDir() => Directory.CreateTempSubdirectory("fmf-settings-").FullName;

    [Fact]
    public void Missing_file_loads_defaults()
    {
        var path = Path.Combine(TempDir(), "settings.json");

        var s = AppSettings.LoadFrom(path);

        Assert.Equal("auto", s.Language);
        Assert.True(s.FocusedSearch);
    }

    [Fact]
    public void Corrupt_file_degrades_to_defaults_and_is_quarantined()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "settings.json");
            File.WriteAllText(path, "{ this is not valid json");

            var s = AppSettings.LoadFrom(path);

            Assert.Equal("auto", s.Language);             // defaults
            Assert.False(File.Exists(path));              // original moved…
            Assert.True(File.Exists(path + ".bad"));      // …to the .bad quarantine
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Unknown_or_obsolete_key_degrades_to_defaults_and_is_quarantined()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "settings.json");
            File.WriteAllText(path, """{"directory_scan_fallback":true}""");

            var settings = AppSettings.LoadFrom(path);

            Assert.Equal("auto", settings.Language);
            Assert.True(settings.FocusedSearch);
            Assert.False(File.Exists(path));
            Assert.True(File.Exists(path + ".bad"));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Oversized_file_degrades_to_defaults_and_is_quarantined()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "settings.json");
            File.WriteAllBytes(path, new byte[(16 * 1024) + 1]);

            var settings = AppSettings.LoadFrom(path);

            Assert.Equal("auto", settings.Language);
            Assert.False(File.Exists(path));
            Assert.True(File.Exists(path + ".bad"));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Save_then_load_roundtrips_non_default_values()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "settings.json");
            var saved = new AppSettings { Language = "ja", FocusedSearch = false };

            Assert.True(saved.SaveTo(path));
            var loaded = AppSettings.LoadFrom(path);

            Assert.Equal("ja", loaded.Language);
            Assert.False(loaded.FocusedSearch);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Save_failure_is_observable_and_cleans_the_staging_file()
    {
        var dir = Directory.CreateTempSubdirectory("fmf-settings-");
        try
        {
            var destinationDirectory = Path.Combine(dir.FullName, "settings.json");
            Directory.CreateDirectory(destinationDirectory);

            var ok = new AppSettings { Language = "ja" }.SaveTo(destinationDirectory);

            Assert.False(ok);
            Assert.True(Directory.Exists(destinationDirectory));
            Assert.Empty(Directory.EnumerateFiles(dir.FullName, "*.tmp"));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }
}
