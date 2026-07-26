using System.Runtime.CompilerServices;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class FfiEngineClientCallbackTests
{
    [Fact]
    public void ManagedEventDispatch_FirewallsEachSubscriberIndependently()
    {
        var client = (FfiEngineClient)RuntimeHelpers.GetUninitializedObject(
            typeof(FfiEngineClient));
        var secondSubscriberCalls = 0;
        client.IndexChanged += _ => throw new InvalidOperationException("scripted");
        client.IndexChanged += _ => secondSubscriberCalls++;

        var exception = Record.Exception(
            () => client.DispatchEventForTests(EventKind.IndexChanged, "C:", 0));

        Assert.Null(exception);
        Assert.Equal(1, secondSubscriberCalls);
    }

    [Fact]
    public async Task SearchAsync_RejectsOversizedQueryBeforeTouchingNativeHandle()
    {
        var client = (FfiEngineClient)RuntimeHelpers.GetUninitializedObject(
            typeof(FfiEngineClient));
        var oversized = new string(
            'x',
            checked((int)EngineContract.MaxQueryBytes + 1));

        await Assert.ThrowsAsync<ArgumentException>(
            () => client.SearchAsync(oversized, SearchOptions.Default));
    }
}
