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
    public void Locked_file_is_never_quarantined_and_saving_is_refused()
    {
        // A backup/sync/AV pass holding settings.json for a moment must not cost
        // the user every setting: the file stays put, and the defaults we fell
        // back to must not be written over it.
        var dir = Directory.CreateTempSubdirectory("fmf-settings-");
        try
        {
            var path = Path.Combine(dir.FullName, "settings.json");
            var original = """{"language":"ja","close_to_tray":true}""";
            File.WriteAllText(path, original);

            AppSettings settings;
            using (File.Open(path, FileMode.Open, FileAccess.Read, FileShare.None))
            {
                settings = AppSettings.LoadFrom(path, NoWait);
            }

            Assert.Equal("auto", settings.Language);              // fallback defaults
            Assert.False(settings.CloseToTray);
            Assert.True(File.Exists(path));                       // …but nothing was moved
            Assert.False(File.Exists(path + ".bad"));

            Assert.False(settings.SaveTo(path));                  // and nothing is written back
            Assert.Equal(original, File.ReadAllText(path));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    [Fact]
    public void Read_is_retried_so_a_momentary_lock_still_loads_the_real_settings()
    {
        var dir = Directory.CreateTempSubdirectory("fmf-settings-");
        try
        {
            var path = Path.Combine(dir.FullName, "settings.json");
            File.WriteAllText(path, """{"language":"ja"}""");

            var exclusive = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.None);
            try
            {
                // The lock clears during the first backoff, exactly like a
                // finished AV/backup pass.
                var settings = AppSettings.LoadFrom(
                    path,
                    _ =>
                    {
                        exclusive.Dispose();
                    });

                Assert.Equal("ja", settings.Language);
                Assert.True(settings.SaveTo(path));
            }
            finally
            {
                exclusive.Dispose();
            }
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    [Fact]
    public void Corrupt_content_is_still_quarantined_and_saving_stays_enabled()
    {
        // The complement of the two tests above: quarantine is reserved for
        // content we did read and cannot use.
        var dir = Directory.CreateTempSubdirectory("fmf-settings-");
        try
        {
            var path = Path.Combine(dir.FullName, "settings.json");
            File.WriteAllText(path, "{ not json");

            var settings = AppSettings.LoadFrom(path, NoWait);

            Assert.False(File.Exists(path));
            Assert.True(File.Exists(path + ".bad"));
            Assert.True(settings.SaveTo(path));
            Assert.True(File.Exists(path));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    /// <summary>Backoff that does not actually wait — the failure the retry
    /// loop is being tested against is permanent for the test's duration.</summary>
    private static void NoWait(int milliseconds) => _ = milliseconds;

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
