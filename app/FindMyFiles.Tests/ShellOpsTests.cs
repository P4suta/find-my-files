using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ShellOpsTests
{
    [Theory]
    [InlineData(@"C:\Users\Public\report.txt")]
    [InlineData(@"C:\My Documents\quarterly report.txt")] // spaces
    [InlineData("C:\\dir\\name with \" quote.txt")] // a Win32-reserved quote — the MFT scan can surface it
    [InlineData(@"C:\dir\a,b /root C:\Windows.txt")] // comma + space + switch-looking text
    [InlineData("C:\\dir\\\" /select,C:\\Windows\\System32\\calc.exe")] // an explorer-switch injection payload
    public void BuildOpenStartInfo_PassesPathAsOneVerbatimArgument(string fullPath)
    {
        var psi = ShellOps.BuildOpenStartInfo(fullPath);

        // The attacker-influenced path must be exactly one argument, byte-for-byte —
        // never split into switches, never folded into the Arguments command line
        // where a '"' could break out and inject (the argument_injection finding).
        Assert.True(string.IsNullOrEmpty(psi.Arguments));
        Assert.Single(psi.ArgumentList);
        Assert.Equal(fullPath, psi.ArgumentList[0]);
        Assert.False(psi.UseShellExecute);
    }

    [Fact]
    public void BuildOpenStartInfo_LaunchesViaSystemExplorer()
    {
        var psi = ShellOps.BuildOpenStartInfo(@"C:\x");

        // De-elevation contract (CLAUDE.md UI固定則): targets open through
        // %WINDIR%\explorer.exe, pinned by full path against binary planting.
        Assert.EndsWith(@"\explorer.exe", psi.FileName, StringComparison.OrdinalIgnoreCase);
    }
}
