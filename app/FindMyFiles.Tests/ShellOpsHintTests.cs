using System.ComponentModel;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Pins the failure hint shell operations attach to their notification. The
/// engine indexes paths up to 32767 UTF-16 units and shell actions pin them
/// through <c>CreateFileW</c>, which reports a path past the legacy MAX_PATH
/// limit as "not found" whenever long-path support is not in effect — the hint
/// must not turn that into "the file was moved or deleted", whose remedy is the
/// opposite (the file is exactly where the user left it). It names the length
/// instead, and points at the machine setting that lifts the limit.
/// </summary>
public sealed class ShellOpsHintTests
{
    private const string ShortPath = @"C:\Users\example\Documents\report.txt";

    private static readonly string LongPath =
        @"C:\Users\example\" + new string('a', 300) + ".txt";

    [Theory]
    [InlineData(2)] // ERROR_FILE_NOT_FOUND
    [InlineData(3)] // ERROR_PATH_NOT_FOUND
    public void A_short_path_that_is_not_found_really_did_move(int error) =>
        Assert.Equal(
            Loc.Get("Shell_HintMoved"),
            ShellOps.Hint(new Win32Exception(error), ShortPath));

    [Theory]
    [InlineData(2)] // ERROR_FILE_NOT_FOUND
    [InlineData(3)] // ERROR_PATH_NOT_FOUND
    [InlineData(206)] // ERROR_FILENAME_EXCED_RANGE
    public void A_long_path_that_is_not_found_blames_its_length(int error) =>
        Assert.Equal(
            Loc.Get("Shell_HintPathTooLong"),
            ShellOps.Hint(new Win32Exception(error), LongPath));

    [Fact]
    public void PathTooLong_blames_its_length() =>
        Assert.Equal(
            Loc.Get("Shell_HintPathTooLong"),
            ShellOps.Hint(new PathTooLongException(), ShortPath));

    [Fact]
    public void Access_denied_still_reports_permissions_on_a_long_path() =>
        Assert.Equal(
            Loc.Get("Shell_HintAccessDenied"),
            ShellOps.Hint(new Win32Exception(5), LongPath));

    [Fact]
    public void Cancelled_elevation_is_reported_as_cancelled() =>
        Assert.Equal(
            Loc.Get("Shell_HintCancelled"),
            ShellOps.Hint(new Win32Exception(1223), ShortPath));

    [Fact]
    public void An_unclassified_failure_keeps_the_recently_moved_hint() =>
        Assert.Equal(
            Loc.Get("Shell_HintMovedRecently"),
            ShellOps.Hint(new IOException("identity mismatch"), ShortPath));
}
