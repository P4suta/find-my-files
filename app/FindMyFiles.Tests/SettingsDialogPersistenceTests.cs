using FindMyFiles.Services;
using FindMyFiles.Views;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class SettingsDialogPersistenceTests
{
    [Fact]
    public void Language_change_is_reverted_when_persistence_fails()
    {
        var settings = new AppSettings { Language = "ja" };

        var saved = SettingsDialog.TryPersistLanguage(
            settings,
            "en",
            save: () => false);

        Assert.False(saved);
        Assert.Equal("ja", settings.Language);
    }

    [Fact]
    public void Language_change_is_kept_only_after_confirmed_persistence()
    {
        var settings = new AppSettings { Language = "ja" };

        var saved = SettingsDialog.TryPersistLanguage(
            settings,
            "en",
            save: () => true);

        Assert.True(saved);
        Assert.Equal("en", settings.Language);
    }
}
