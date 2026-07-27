using System.Diagnostics;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ShellOpsTests : IDisposable
{
    // The relaunch failure-path tests below post to the process-wide Notifier
    // (ShellOps.Run reports swallowed failures); reset it on teardown so a post
    // can't replay into another test's ViewModel. DisableTestParallelization
    // (see TestParallelization.cs) makes this reset deterministic.
    public void Dispose()
    {
        Notifier.ResetForTests();
        GC.SuppressFinalize(this);
    }

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

        // De-elevation contract (AGENTS.md UI invariants): targets open through
        // %WINDIR%\explorer.exe, pinned by full path against binary planting.
        Assert.EndsWith(@"\explorer.exe", psi.FileName, StringComparison.OrdinalIgnoreCase);
    }

    private sealed class RecordingRunner : IProcessRunner
    {
        internal ProcessStartInfo? Started { get; private set; }

        internal int Calls { get; private set; }

        public void Start(ProcessStartInfo psi)
        {
            Calls++;
            Started = psi;
        }
    }

    private sealed class RecordingVerifier : IIndexedShellTargetVerifier
    {
        internal string? Path { get; private set; }

        internal ulong Frn { get; private set; }

        internal bool IsPinned { get; private set; }

        internal bool WasDisposed { get; private set; }

        public IDisposable VerifyAndPin(string fullPath, ulong expectedFrn)
        {
            Path = fullPath;
            Frn = expectedFrn;
            IsPinned = true;
            return new CallbackLease(() =>
            {
                IsPinned = false;
                WasDisposed = true;
            });
        }
    }

    private sealed class CallbackLease(Action dispose) : IDisposable
    {
        private Action? _dispose = dispose;

        public void Dispose() => Interlocked.Exchange(ref _dispose, null)?.Invoke();
    }

    private sealed class PinAssertingRunner(RecordingVerifier verifier) : IProcessRunner
    {
        internal int Calls { get; private set; }

        public void Start(ProcessStartInfo psi)
        {
            Assert.True(verifier.IsPinned);
            Calls++;
        }
    }

    private sealed class ThrowingVerifier : IIndexedShellTargetVerifier
    {
        public IDisposable VerifyAndPin(string fullPath, ulong expectedFrn) =>
            throw new IOException("identity mismatch");
    }

    [Fact]
    public void OpenTrustedWith_drives_the_runner_with_the_path_as_one_verbatim_argument()
    {
        // "Open" used to call Process.Start directly, so nothing verified that the
        // built start info ever reached a launch. Drive a fake runner and assert it.
        var runner = new RecordingRunner();

        ShellOps.OpenTrustedWith(runner, "C:\\dir\\name with \" quote.txt");

        Assert.Equal(1, runner.Calls);
        Assert.NotNull(runner.Started);
        Assert.Single(runner.Started!.ArgumentList);
        Assert.Equal("C:\\dir\\name with \" quote.txt", runner.Started.ArgumentList[0]);
    }

    [Fact]
    public void OpenIndexedWith_pins_exact_identity_through_dispatch_and_disposes()
    {
        var verifier = new RecordingVerifier();
        var runner = new PinAssertingRunner(verifier);

        ShellOps.OpenIndexedWith(verifier, runner, @"C:\dir\file.txt", 0x0007_0000_0000_0042);

        Assert.Equal(1, runner.Calls);
        Assert.Equal(@"C:\dir\file.txt", verifier.Path);
        Assert.Equal(0x0007_0000_0000_0042UL, verifier.Frn);
        Assert.False(verifier.IsPinned);
        Assert.True(verifier.WasDisposed);
    }

    [Fact]
    public void OpenIndexedWith_never_dispatches_when_identity_cannot_be_proved()
    {
        var runner = new RecordingRunner();

        ShellOps.OpenIndexedWith(
            new ThrowingVerifier(),
            runner,
            @"C:\dir\file.txt",
            0x0007_0000_0000_0042);

        Assert.Equal(0, runner.Calls);
    }

    [Theory]
    [InlineData(@"C:\dir\file.txt", true)]
    [InlineData(@"z:\名前\😀.txt", true)]
    [InlineData(@"C:\dir", true)]
    [InlineData(@"C:\dir\", false)]
    [InlineData(@"C:\dir\\file.txt", false)]
    [InlineData(@"C:\dir\.", false)]
    [InlineData(@"C:\dir\..", false)]
    [InlineData(@"C:\dir\trailing.", false)]
    [InlineData(@"C:\dir\trailing ", false)]
    [InlineData(@"C:\dir\NUL.txt", false)]
    [InlineData(@"C:\dir\CONIN$", false)]
    [InlineData(@"C:\dir\COM1 .txt", false)]
    [InlineData(@"C:\dir\COM1  .txt", false)] // several trailing spaces still resolve to COM1
    [InlineData(@"C:\dir\report .txt", true)] // ordinary name: a space before the dot is not a device
    [InlineData(@"C:\dir\notes .d\report .txt", true)] // ... in a directory component either
    [InlineData(@"C:\dir\COMMON .txt", true)] // COM-prefixed but not a device stem
    [InlineData(@"C:\dir\COM¹.log", false)]
    [InlineData(@"C:\dir\stream:name", false)]
    [InlineData(@"C:\dir\question?.txt", false)]
    [InlineData(@"C:/dir/file.txt", false)]
    [InlineData(@"\\server\share\file.txt", false)]
    [InlineData(@"\\?\C:\dir\file.txt", false)]
    [InlineData(@"relative\file.txt", false)]
    public void Indexed_shell_path_requires_one_unambiguous_Win32_spelling(
        string path,
        bool expected) =>
        Assert.Equal(expected, RealIndexedShellTargetVerifier.IsLexicallySafe(path));

    [Fact]
    public void Pinned_leaf_cannot_be_deleted_or_renamed_while_the_lease_is_held()
    {
        // The only proof that the lease is a lease: an attributes-only open does
        // not take part in the Win32 share check, so withholding FILE_SHARE_DELETE
        // used to be decorative and the leaf could be swapped between the identity
        // check and shell dispatch. Real files, real handles — a fake verifier
        // could not fail this.
        var dir = Path.Combine(Path.GetTempPath(), "fmf-pin-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, "pinned.txt");
        var swapped = Path.Combine(dir, "swapped.txt");
        File.WriteAllText(file, "indexed");
        try
        {
            // Reaching a lease at all also pins the fallback path: %TEMP% sits under
            // C:\ and C:\Users, which withhold DELETE from a standard user.
            using (RealIndexedShellTargetVerifier.Instance.VerifyAndPin(
                file,
                RealIndexedShellTargetVerifier.ReadFileReference(file)))
            {
                var delete = Record.Exception(() => File.Delete(file));
                var rename = Record.Exception(() => File.Move(file, swapped));

                Assert.True(
                    delete is IOException or UnauthorizedAccessException,
                    $"delete of a pinned leaf must fail, got: {delete?.GetType().Name ?? "success"}");
                Assert.True(
                    rename is IOException or UnauthorizedAccessException,
                    $"rename of a pinned leaf must fail, got: {rename?.GetType().Name ?? "success"}");
                Assert.True(File.Exists(file));
                Assert.False(File.Exists(swapped));
            }

            // The lock lives exactly as long as the lease, not one call longer.
            File.Delete(file);
            Assert.False(File.Exists(file));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Replacing_the_leaf_between_index_and_use_fails_the_identity_check()
    {
        var dir = Path.Combine(Path.GetTempPath(), "fmf-pin-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, "target.txt");
        File.WriteAllText(file, "indexed");
        try
        {
            ulong indexedFrn = RealIndexedShellTargetVerifier.ReadFileReference(file);

            // Same path, different NTFS object (the sequence number moves even when
            // the record is reused) — exactly the swap the FRN check exists for.
            File.Delete(file);
            File.WriteAllText(file, "attacker");

            Assert.ThrowsAny<IOException>(
                () => RealIndexedShellTargetVerifier.Instance.VerifyAndPin(file, indexedFrn));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Verifier_failures_speak_the_user_interface_language()
    {
        // These messages reach the notification body verbatim through
        // ShellOps.ReportFailure, so they must resolve through Loc like every
        // other shell string — not be hardcoded English.
        var ex = Assert.Throws<InvalidOperationException>(
            () => RealIndexedShellTargetVerifier.Instance.VerifyAndPin(@"C:\dir\COM1", 1));

        Assert.Equal(Loc.Get("Shell_UnsafeIndexedName"), ex.Message);
        Assert.NotEqual("Shell_UnsafeIndexedName", ex.Message); // the key resolves in every locale
    }

    // FILE_ATTRIBUTE_REPARSE_POINT (0x400) plus the attribute a real entry also
    // carries (directory 0x10 / archive 0x20), exactly as FILE_ATTRIBUTE_TAG_INFO
    // reports them.
    [Theory]
    [InlineData(0x00000020u, 0x00000000u, false)] // ordinary file
    [InlineData(0x00000010u, 0x00000000u, false)] // ordinary directory
    [InlineData(0x00000410u, 0xA0000003u, true)] // IO_REPARSE_TAG_MOUNT_POINT (junction)
    [InlineData(0x00000410u, 0xA000000Cu, true)] // IO_REPARSE_TAG_SYMLINK
    [InlineData(0x00000410u, 0xA0000019u, true)] // IO_REPARSE_TAG_GLOBAL_REPARSE (surrogate)
    [InlineData(0x00000410u, 0x00000000u, true)] // reparse point with no tag: unclassifiable
    [InlineData(0x00000420u, 0x9000001Au, false)] // IO_REPARSE_TAG_CLOUD (OneDrive placeholder)
    [InlineData(0x00000410u, 0x9000101Au, false)] // IO_REPARSE_TAG_CLOUD_1 (a sync root)
    [InlineData(0x00000420u, 0x9000901Au, false)] // IO_REPARSE_TAG_CLOUD_9
    [InlineData(0x00000420u, 0x80000015u, false)] // IO_REPARSE_TAG_FILE_PLACEHOLDER
    [InlineData(0x00000420u, 0x80000013u, false)] // IO_REPARSE_TAG_DEDUP
    [InlineData(0x00000420u, 0x80000017u, false)] // IO_REPARSE_TAG_WOF
    [InlineData(0x00000420u, 0xC0000004u, false)] // IO_REPARSE_TAG_HSM
    [InlineData(0x00000420u, 0x8000001Bu, false)] // IO_REPARSE_TAG_APPEXECLINK
    [InlineData(0x00000020u, 0xA0000003u, false)] // no reparse attribute: the tag field is stale noise
    public void Only_name_surrogate_reparse_points_redirect_path_resolution(
        uint fileAttributes,
        uint reparseTag,
        bool expected)
    {
        // Blanket-rejecting FILE_ATTRIBUTE_REPARSE_POINT broke every result under
        // OneDrive — placeholders and sync roots are reparse points, and Files
        // On-Demand is on by default since Windows 10 1809, so Known-Folder-Moved
        // Desktop/Documents/Pictures were entirely undispatchable. Those tags leave
        // the object where it is; only the name-surrogate bit (0x2000_0000) makes a
        // path resolve elsewhere, which is what the check must key on. These tag
        // values cannot be created on a test machine, so they are pinned here.
        Assert.Equal(
            expected,
            RealIndexedShellTargetVerifier.RedirectsPathResolution(fileAttributes, reparseTag));
    }

    [Fact]
    public void A_junction_in_the_path_is_rejected_while_the_true_path_verifies()
    {
        // Real reparse points on the real file system: the pure classifier above
        // is only worth anything if the verifier actually consults it, and a
        // junction is the one name surrogate a non-elevated test can create.
        var root = Path.Combine(Path.GetTempPath(), "fmf-junction-" + Guid.NewGuid().ToString("N"));
        var real = Path.Combine(root, "real");
        Directory.CreateDirectory(real);
        var file = Path.Combine(real, "target.txt");
        File.WriteAllText(file, "indexed");
        var junction = Path.Combine(root, "link");
        try
        {
            NativeJunction.Create(junction, real);
            ulong frn = RealIndexedShellTargetVerifier.ReadFileReference(file);

            // Control: the same leaf, reached by its own name, still verifies and
            // pins — the rejection below must come from the junction, not from
            // some incidental property of the fixture.
            RealIndexedShellTargetVerifier.Instance.VerifyAndPin(file, frn).Dispose();

            var ex = Assert.Throws<InvalidOperationException>(() =>
                RealIndexedShellTargetVerifier.Instance.VerifyAndPin(
                    Path.Combine(junction, "target.txt"),
                    frn));

            // The junction resolves to the very same FRN, so the identity check
            // cannot catch this: the reparse rule is the only thing that does.
            Assert.Equal(Loc.Get("Shell_ReparsePointBlocked"), ex.Message);
            Assert.NotEqual("Shell_ReparsePointBlocked", ex.Message);
        }
        finally
        {
            // Drop the junction by itself first. A recursive delete tries to
            // unmount an IO_REPARSE_TAG_MOUNT_POINT entry before unlinking it,
            // and DeleteVolumeMountPoint fails with ACCESS_DENIED on a junction
            // that names a directory rather than a volume; a plain
            // RemoveDirectory unlinks the junction without touching its target.
            if (Directory.Exists(junction))
            {
                Directory.Delete(junction);
            }

            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void Indexed_shell_path_rejects_unpaired_surrogates()
    {
        string path = "C:\\dir\\" + '\uD800' + ".txt";

        Assert.False(RealIndexedShellTargetVerifier.IsLexicallySafe(path));
    }

    private sealed class RecordingRestart : IAppRestart
    {
        internal int Calls { get; private set; }

        internal string? LastArguments { get; private set; }

        public void Restart(string arguments)
        {
            Calls++;
            LastArguments = arguments;
        }
    }

    private sealed class ThrowingRestart : IAppRestart
    {
        // Models a failed restart: AppInstance.Restart returns a failure reason,
        // which RealAppRestart surfaces as an exception for ShellOps.Run to catch.
        public void Restart(string arguments) =>
            throw new InvalidOperationException("restart failed");
    }

    [Fact]
    public void RelaunchWith_restarts_the_app_with_empty_arguments()
    {
        // The language switch is the only true restart left (ADR-0036): it must hand
        // the fresh instance an empty command line, so settings.json's saved language
        // drives the new instance rather than a stale --engine override.
        var restart = new RecordingRestart();

        ShellOps.RelaunchWith(restart);

        Assert.Equal(1, restart.Calls);
        Assert.Equal(string.Empty, restart.LastArguments);
    }

    [Fact]
    public void RelaunchWith_swallows_a_failed_restart_instead_of_throwing()
    {
        // A restart failure is funneled through ShellOps.Run (notify, don't crash):
        // the call must return normally rather than propagate — Dispose resets the
        // Notifier the swallowed failure posted to.
        ShellOps.RelaunchWith(new ThrowingRestart());
    }
}
