using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="ShellOps.DoReveal"/> — the
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

    [Fact]
    public void Success_returns_null_and_frees_the_pidl()
    {
        var api = new FakeRevealApi(parseHr: 0, openHr: 0);

        Assert.Null(ShellOps.DoReveal(api, @"C:\dir\file.txt"));
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

        Assert.NotNull(ShellOps.DoReveal(api, @"C:\dir\file.txt"));
        Assert.Equal(1, api.FreeCalls);
    }

    [Fact]
    public void Negative_open_hr_is_a_failure_and_still_frees()
    {
        var api = new FakeRevealApi(parseHr: 0, openHr: unchecked((int)0x80004005)); // E_FAIL

        Assert.NotNull(ShellOps.DoReveal(api, @"C:\dir\file.txt"));
        Assert.Equal(1, api.FreeCalls);
    }

    [Fact]
    public void Parse_failure_skips_open_and_frees_nothing()
    {
        var api = new FakeRevealApi(parseHr: unchecked((int)0x80070002), openHr: 0); // ERROR_FILE_NOT_FOUND

        Assert.NotNull(ShellOps.DoReveal(api, @"C:\missing\file.txt"));
        Assert.Equal(0, api.OpenCalls);
        Assert.Equal(0, api.FreeCalls);
    }
}
