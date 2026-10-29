using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests.Contract;

public sealed class FixtureLifetimeTests(ITestOutputHelper output)
{
    [Fact]
    public async Task FailedInitializationReleasesTheFixtureAndPreservesTheFailure()
    {
        var failure = new IOException("fixture initialization failed");
        var fixture = new FailedFixture(output, failure, cleanupFailure: null);

        var actual = await Assert.ThrowsAsync<IOException>(
            () => fixture.InitializeAsync().AsTask());

        Assert.Same(failure, actual);
        Assert.Equal(1, fixture.CleanupCalls);
    }

    [Fact]
    public async Task FailedCleanupPreservesBothInitializationAndCleanupFailures()
    {
        var primary = new IOException("fixture initialization failed");
        var secondary = new IOException("fixture cleanup failed");
        var fixture = new FailedFixture(output, primary, secondary);

        var actual = await Assert.ThrowsAsync<AggregateException>(
            () => fixture.InitializeAsync().AsTask());

        Assert.Collection(
            actual.InnerExceptions,
            error => Assert.Same(primary, error),
            error => Assert.Same(secondary, error));
        Assert.Equal(1, fixture.CleanupCalls);
    }

    private sealed class FailedFixture(
        ITestOutputHelper output,
        Exception failure,
        Exception? cleanupFailure) : EngineClientContractTests(output)
    {
        public int CleanupCalls { get; private set; }

        protected override string ValidQuery => "fixture";

        private protected override Task<IEngineClient?> AcquireClientOrSkipAsync() =>
            Task.FromException<IEngineClient?>(failure);

        protected override Task TeardownAsync()
        {
            CleanupCalls++;
            return cleanupFailure is null
                ? Task.CompletedTask
                : Task.FromException(cleanupFailure);
        }
    }
}
