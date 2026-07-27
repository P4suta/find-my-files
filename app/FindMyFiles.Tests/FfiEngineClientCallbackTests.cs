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
    public void ManagedEventDispatch_RejectsInvalidEngineErrorSeverity()
    {
        var client = (FfiEngineClient)RuntimeHelpers.GetUninitializedObject(
            typeof(FfiEngineClient));
        var calls = 0;
        client.EngineErrorOccurred += _ => calls++;

        var exception = Record.Exception(
            () => client.DispatchEventForTests(
                EventKind.EngineError,
                "C:",
                ulong.MaxValue));

        Assert.Null(exception);
        Assert.Equal(0, calls);
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

    [Fact]
    public void PresentationBasis_IsScopedToOneFfiEngineSession()
    {
        var owner = new object();
        var foreignOwner = new object();
        using var result = FfiSearchResult.CreateNonOwningForTests(
            new IntPtr(0x1234),
            owner);

        Assert.False(result.TryAcquirePresentationBasis(
            foreignOwner,
            out var added,
            out var foreignId));
        Assert.False(added);
        Assert.Equal(0UL, foreignId);

        Assert.True(result.TryAcquirePresentationBasis(
            owner,
            out added,
            out var id));
        Assert.True(added);
        Assert.Equal(0x1234UL, id);
        result.DangerousRelease();

        result.Dispose();
        Assert.False(result.TryAcquirePresentationBasis(
            owner,
            out added,
            out var disposedId));
        Assert.False(added);
        Assert.Equal(0UL, disposedId);
    }
}
