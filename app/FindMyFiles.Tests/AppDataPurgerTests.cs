using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class AppDataPurgerTests
{
    [Fact]
    public void Purge_closes_the_log_then_removes_the_whole_user_tree()
    {
        var dir = Directory.CreateTempSubdirectory("fmf-user-purge-");
        try
        {
            var root = Path.Combine(dir.FullName, "find-my-files");
            Directory.CreateDirectory(Path.Combine(root, "logs"));
            File.WriteAllText(Path.Combine(root, "settings.json"), "{}");
            File.WriteAllText(Path.Combine(root, "logs", "app.log"), "line");
            var logClosed = false;

            var ok = AppDataPurger.TryPurge(root, () => logClosed = true);

            Assert.True(ok);
            Assert.True(logClosed);
            Assert.False(Directory.Exists(root));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    [Fact]
    public void Purge_reports_failure_instead_of_claiming_partial_cleanup()
    {
        var dir = Directory.CreateTempSubdirectory("fmf-user-purge-");
        try
        {
            var root = Path.Combine(dir.FullName, "find-my-files");
            Directory.CreateDirectory(root);

            var ok = AppDataPurger.TryPurge(
                root,
                () => throw new IOException("injected log-close failure"));

            Assert.False(ok);
            Assert.True(Directory.Exists(root));
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }
}
