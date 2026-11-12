using FindMyFiles.Tests.TestDoubles;
using Xunit;
using Xunit.Sdk;

namespace FindMyFiles.Tests;

public sealed class PendingRequestFixtureTests
{
    [Fact]
    public async Task Failure_settles_and_observes_an_unfinished_request()
    {
        var response = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var request = (Task)response.Task;
        var pending = new PendingRequestFixture(request, response, TimeSpan.Zero);
        try
        {
            await Assert.ThrowsAsync<SameException>(pending.AssertCompletesAsync);

            await pending.DisposeAsync();

            Assert.True(request.IsCompleted);
            Assert.True(response.Task.IsCanceled);
        }
        finally
        {
            response.TrySetCanceled(TestContext.Current.CancellationToken);
            await request.ConfigureAwait(
                ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
            await pending.DisposeAsync();
        }
    }
}
