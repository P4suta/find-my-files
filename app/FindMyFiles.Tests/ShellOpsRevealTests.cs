using System.Runtime.InteropServices;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="ShellOps.DoRevealIndexed"/> — the
/// reveal-and-select orchestration that shipped broken because only the pure
/// <c>BuildOpenStartInfo</c> helper was tested, never the HRESULT handling.
/// A fake <see cref="IRevealApi"/> drives every branch without a live shell.</summary>
public sealed class ShellOpsRevealTests
{
    private sealed class FakeRevealApi(int parseHr, int openHr) : IRevealApi
    {
        internal int OpenCalls { get; private set; }

        internal int FreeCalls { get; private set; }

        public int ParseDisplayName(string path, out IntPtr pidl)
        {
            pidl = parseHr == 0 ? (IntPtr)0xABCD : IntPtr.Zero;
            return parseHr;
        }

        public int OpenFolderAndSelectItems(IntPtr pidl)
        {
            OpenCalls++;
            return openHr;
        }

        public void FreePidl(IntPtr pidl) => FreeCalls++;
    }

    private sealed class RecordingVerifier : IIndexedShellTargetVerifier
    {
        internal bool IsPinned { get; private set; }

        internal bool WasDisposed { get; private set; }

        public IDisposable VerifyAndPin(string fullPath, ulong expectedFrn)
        {
            IsPinned = true;
            return new Lease(this);
        }

        private sealed class Lease(RecordingVerifier owner) : IDisposable
        {
            public void Dispose()
            {
                owner.IsPinned = false;
                owner.WasDisposed = true;
            }
        }
    }

    private sealed class PinAssertingRevealApi(
        RecordingVerifier verifier,
        int parseHr = 0,
        int openHr = 0) : IRevealApi
    {
        internal int ParseCalls { get; private set; }

        public int ParseDisplayName(string path, out IntPtr pidl)
        {
            Assert.True(verifier.IsPinned);
            ParseCalls++;
            pidl = parseHr == 0 ? (IntPtr)0xABCD : IntPtr.Zero;
            return parseHr;
        }

        public int OpenFolderAndSelectItems(IntPtr pidl)
        {
            Assert.True(verifier.IsPinned);
            return openHr;
        }

        public void FreePidl(IntPtr pidl) => Assert.True(verifier.IsPinned);
    }

    private sealed class ThrowingVerifier : IIndexedShellTargetVerifier
    {
        public IDisposable VerifyAndPin(string fullPath, ulong expectedFrn) =>
            throw new IOException("identity mismatch");
    }

    [Fact]
    public void Success_returns_null_and_frees_the_pidl()
    {
        var api = new FakeRevealApi(parseHr: 0, openHr: 0);

        Assert.Null(ShellOps.DoRevealIndexed(
            new RecordingVerifier(),
            api,
            @"C:\dir\file.txt",
            0x0007_0000_0000_0042));
        Assert.Equal(1, api.OpenCalls);
        Assert.Equal(1, api.FreeCalls);
    }

    [Fact]
    public void Non_negative_open_hr_is_a_failure_and_still_frees()
    {
        // S_FALSE (1) has its severity bit clear, so Marshal.ThrowExceptionForHR
        // (the old code) treated it as success — reveal silently did nothing.
        // This is the regression test that pins the shipped-broken behaviour.
        var api = new FakeRevealApi(parseHr: 0, openHr: 1);

        var failure = Assert.IsType<InvalidOperationException>(ShellOps.DoRevealIndexed(
            new RecordingVerifier(),
            api,
            @"C:\dir\file.txt",
            0x0007_0000_0000_0042));
        Assert.Equal(
            "reveal failed (SHOpenFolderAndSelectItems returned 0x00000001)",
            failure.Message);
        Assert.Equal(1, api.FreeCalls);
    }

    [Fact]
    public void Negative_open_hr_is_a_failure_and_still_frees()
    {
        var api = new FakeRevealApi(parseHr: 0, openHr: unchecked((int)0x80004005)); // E_FAIL

        var failure = Assert.IsType<COMException>(ShellOps.DoRevealIndexed(
            new RecordingVerifier(),
            api,
            @"C:\dir\file.txt",
            0x0007_0000_0000_0042));
        Assert.Equal(unchecked((int)0x80004005), failure.HResult);
        Assert.Equal(1, api.FreeCalls);
    }

    [Fact]
    public void Parse_failure_skips_open_and_frees_nothing()
    {
        var api = new FakeRevealApi(parseHr: unchecked((int)0x80070002), openHr: 0); // ERROR_FILE_NOT_FOUND

        var failure = Assert.IsAssignableFrom<IOException>(ShellOps.DoRevealIndexed(
            new RecordingVerifier(),
            api,
            @"C:\missing\file.txt",
            0x0007_0000_0000_0042));
        Assert.Equal(unchecked((int)0x80070002), failure.HResult);
        Assert.Equal(0, api.OpenCalls);
        Assert.Equal(0, api.FreeCalls);
    }

    [Fact]
    public void Indexed_reveal_keeps_identity_pinned_through_shell_and_disposes()
    {
        var verifier = new RecordingVerifier();
        var api = new PinAssertingRevealApi(verifier);

        Assert.Null(ShellOps.DoRevealIndexed(
            verifier,
            api,
            @"C:\dir\file.txt",
            0x0007_0000_0000_0042));
        Assert.Equal(1, api.ParseCalls);
        Assert.False(verifier.IsPinned);
        Assert.True(verifier.WasDisposed);
    }

    [Fact]
    public void Indexed_reveal_never_reaches_shell_when_identity_cannot_be_proved()
    {
        var api = new FakeRevealApi(parseHr: 0, openHr: 0);

        Assert.NotNull(ShellOps.DoRevealIndexed(
            new ThrowingVerifier(),
            api,
            @"C:\dir\file.txt",
            0x0007_0000_0000_0042));
        Assert.Equal(0, api.OpenCalls);
        Assert.Equal(0, api.FreeCalls);
    }
}
