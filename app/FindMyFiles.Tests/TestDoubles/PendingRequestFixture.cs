using System.Reflection;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests.TestDoubles;

internal sealed class PendingRequestFixture : IAsyncDisposable
{
    private readonly Task _request;
    private readonly TaskCompletionSource<(int Status, byte[] Payload)> _response;
    private readonly TimeSpan _completionTimeout;

    internal PendingRequestFixture(
        Task request,
        TaskCompletionSource<(int Status, byte[] Payload)> response,
        TimeSpan completionTimeout)
    {
        _request = request;
        _response = response;
        _completionTimeout = completionTimeout;
    }

    internal static PendingRequestFixture CaptureSingle(PipeEngineClient client, Task request) =>
        new(request, CaptureResponse(client), TimeSpan.FromSeconds(5));

    internal static TaskCompletionSource<(int Status, byte[] Payload)> CaptureResponse(
        PipeEngineClient client)
    {
        const BindingFlags privateInstance = BindingFlags.Instance | BindingFlags.NonPublic;
        var requests = Assert.IsAssignableFrom<System.Collections.IDictionary>(
            typeof(PipeEngineClient).GetField("_pending", privateInstance)?.GetValue(client));
        var pending = Assert.Single(requests.Values.Cast<object>());
        return Assert.IsType<TaskCompletionSource<(int Status, byte[] Payload)>>(
            pending.GetType().GetProperty("Completion")?.GetValue(pending));
    }

    internal async Task AssertCompletesAsync()
    {
        var completed = await Task.WhenAny(
            _request,
            Task.Delay(_completionTimeout, TestContext.Current.CancellationToken));
        Assert.Same(_request, completed);
    }

    public async ValueTask DisposeAsync()
    {
        _response.TrySetCanceled();
        await ((Task)_response.Task).ConfigureAwait(
            ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
        await ObserveAsync(_request);
        GC.SuppressFinalize(this);
    }

    internal static async Task ObserveAsync(Task request)
    {
        await request.WaitAsync(TimeSpan.FromSeconds(5), CancellationToken.None)
            .ConfigureAwait(
                ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
        Assert.True(request.IsCompleted);
    }
}
