using System.Diagnostics;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ShellOpsTests : IDisposable
{
    public void Dispose()
    {
        Notifier.ResetForTests();
        GC.SuppressFinalize(this);
    }

    [Theory]
    [InlineData(@"C:\Users\Public\report.txt")]
    [InlineData(@"C:\My Documents\quarterly report.txt")]
    [InlineData("C:\\dir\\name with \" quote.txt")]
    [InlineData(@"C:\dir\a,b /root C:\Windows.txt")]
    [InlineData("C:\\dir\\\" /select,C:\\Windows\\System32\\calc.exe")]
    public void BuildOpenStartInfo_PassesPathAsOneVerbatimArgument(string fullPath)
    {
        var psi = ShellOps.BuildOpenStartInfo(fullPath);

        Assert.True(string.IsNullOrEmpty(psi.Arguments));
        Assert.Single(psi.ArgumentList);
        Assert.Equal(fullPath, psi.ArgumentList[0]);
        Assert.False(psi.UseShellExecute);
    }

    [Fact]
    public void BuildOpenStartInfo_LaunchesViaSystemExplorer()
    {
        var psi = ShellOps.BuildOpenStartInfo(@"C:\x");

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

    private sealed class ThrowingRunner : IProcessRunner
    {
        public void Start(ProcessStartInfo psi) =>
            throw new InvalidOperationException("launch failed");
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

    private sealed class SuccessfulRevealApi : IRevealApi
    {
        public int ParseDisplayName(string path, out IntPtr pidl)
        {
            pidl = (IntPtr)0x1234;
            return 0;
        }

        public int OpenFolderAndSelectItems(IntPtr pidl) => 0;

        public void FreePidl(IntPtr pidl)
        {
        }
    }

    private static ShellOpsHooks Hooks(
        IProcessRunner runner,
        IIndexedShellTargetVerifier verifier,
        IAppRestart restart,
        IRevealApi? reveal = null,
        Action<Action>? startSta = null,
        Func<int>? coInitialize = null,
        Action? coUninitialize = null,
        Action<string>? copyText = null) =>
        new(
            runner,
            verifier,
            reveal ?? new SuccessfulRevealApi(),
            restart,
            startSta ?? (static action => action()),
            coInitialize ?? (static () => 0),
            coUninitialize ?? (static () => { }),
            copyText ?? (static _ => { }));

    [Fact]
    public void OpenTrustedWith_drives_the_runner_with_the_path_as_one_verbatim_argument()
    {
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

    [Fact]
    public void Public_shell_entry_points_use_the_atomic_boundary_set()
    {
        var runner = new RecordingRunner();
        var verifier = new RecordingVerifier();
        var restart = new RecordingRestart();
        var staStarts = 0;
        var coInitializes = 0;
        var coUninitializes = 0;
        string? copied = null;
        using var scope = ShellOps.UseHooksForTests(Hooks(
            runner,
            verifier,
            restart,
            startSta: action =>
            {
                staStarts++;
                action();
            },
            coInitialize: () =>
            {
                coInitializes++;
                return 0;
            },
            coUninitialize: () => coUninitializes++,
            copyText: text => copied = text));

        ShellOps.OpenTrusted(@"C:\dir\open.txt");
        ShellOps.OpenIndexed(@"C:\dir\indexed.txt", 42);
        ShellOps.RevealIndexed(@"C:\dir\reveal.txt", 43);
        ShellOps.Relaunch();
        ShellOps.CopyText("copied text", "test");

        Assert.Equal(2, runner.Calls);
        Assert.Equal(@"C:\dir\reveal.txt", verifier.Path);
        Assert.Equal(43UL, verifier.Frn);
        Assert.Equal(1, staStarts);
        Assert.Equal(1, coInitializes);
        Assert.Equal(1, coUninitializes);
        Assert.Equal(1, restart.Calls);
        Assert.Equal("copied text", copied);
    }

    [Fact]
    public void Reveal_failure_notifies_and_a_failed_Com_init_is_not_uninitialized()
    {
        var notifications = new List<AppNotification>();
        using var subscription = Notifier.Attach(notifications.Add);
        var uninitializes = 0;
        using var scope = ShellOps.UseHooksForTests(Hooks(
            new RecordingRunner(),
            new ThrowingVerifier(),
            new RecordingRestart(),
            coInitialize: () => -1,
            coUninitialize: () => uninitializes++));

        ShellOps.RevealIndexed(@"C:\dir\missing.txt", 44);

        var notification = Assert.Single(notifications);
        Assert.Equal(NotifySeverity.Warning, notification.Severity);
        Assert.Contains("identity mismatch", notification.Detail, StringComparison.Ordinal);
        Assert.Equal(0, uninitializes);
    }

    [Fact]
    public void Reveal_contains_a_Com_initialization_exception_on_the_STA_thread()
    {
        using var log = new LogCapture();
        var notifications = new List<AppNotification>();
        using var subscription = Notifier.Attach(notifications.Add);
        var uninitializes = 0;
        using var scope = ShellOps.UseHooksForTests(Hooks(
            new RecordingRunner(),
            new RecordingVerifier(),
            new RecordingRestart(),
            coInitialize: () => throw new InvalidOperationException("COM unavailable"),
            coUninitialize: () => uninitializes++));

        ShellOps.RevealIndexed(@"C:\dir\file.txt", 45);

        var notification = Assert.Single(notifications);
        Assert.Equal(NotifySeverity.Warning, notification.Severity);
        Assert.Contains("COM unavailable", notification.Detail, StringComparison.Ordinal);
        Assert.Equal(0, uninitializes);
        Assert.Contains(
            log.Text.Split('\n'),
            line => line.Contains("shell operation failed", StringComparison.Ordinal)
                && line.Split(' ', StringSplitOptions.RemoveEmptyEntries)
                    .Contains("operation=reveal", StringComparer.Ordinal));
    }

    [Fact]
    public void Clipboard_failure_is_reported_without_escaping()
    {
        var notifications = new List<AppNotification>();
        using var subscription = Notifier.Attach(notifications.Add);
        using var scope = ShellOps.UseHooksForTests(Hooks(
            new RecordingRunner(),
            new RecordingVerifier(),
            new RecordingRestart(),
            copyText: _ => throw new InvalidOperationException("clipboard busy")));

        ShellOps.CopyText("text", "diagnostics");

        var notification = Assert.Single(notifications);
        Assert.Equal(NotifySeverity.Warning, notification.Severity);
        Assert.Contains("clipboard busy", notification.Detail, StringComparison.Ordinal);
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
    [InlineData(@"C:\dir\COM1  .txt", false)]
    [InlineData(@"C:\dir\report .txt", true)]
    [InlineData(@"C:\dir\notes .d\report .txt", true)]
    [InlineData(@"C:\dir\COMMON .txt", true)]
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

    private static string CreateNtfsFixtureDirectory(string name)
    {
        var root = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Temp");
        Assert.True(Path.IsPathFullyQualified(root));
        Assert.Equal("NTFS", new DriveInfo(new DirectoryInfo(root).Root.FullName).DriveFormat);
        var directory = Path.Combine(root, "fmf-" + name + "-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        return directory;
    }

    [Fact]
    public void Pinned_leaf_cannot_be_deleted_or_renamed_while_the_lease_is_held()
    {
        var dir = CreateNtfsFixtureDirectory("pin");
        var file = Path.Combine(dir, "pinned.txt");
        var swapped = Path.Combine(dir, "swapped.txt");
        File.WriteAllText(file, "indexed");
        try
        {
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
        var dir = CreateNtfsFixtureDirectory("pin");
        var file = Path.Combine(dir, "target.txt");
        File.WriteAllText(file, "indexed");
        try
        {
            ulong indexedFrn = RealIndexedShellTargetVerifier.ReadFileReference(file);

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
        var ex = Assert.Throws<InvalidOperationException>(
            () => RealIndexedShellTargetVerifier.Instance.VerifyAndPin(@"C:\dir\COM1", 1));

        Assert.Equal(Loc.Get("Shell_UnsafeIndexedName"), ex.Message);
        Assert.NotEqual("Shell_UnsafeIndexedName", ex.Message);
    }

    [Theory]
    [InlineData(0x00000020u, 0x00000000u, false)]
    [InlineData(0x00000010u, 0x00000000u, false)]
    [InlineData(0x00000410u, 0xA0000003u, true)]
    [InlineData(0x00000410u, 0xA000000Cu, true)]
    [InlineData(0x00000410u, 0xA0000019u, true)]
    [InlineData(0x00000410u, 0x00000000u, true)]
    [InlineData(0x00000420u, 0x9000001Au, false)]
    [InlineData(0x00000410u, 0x9000101Au, false)]
    [InlineData(0x00000420u, 0x9000901Au, false)]
    [InlineData(0x00000420u, 0x80000015u, false)]
    [InlineData(0x00000420u, 0x80000013u, false)]
    [InlineData(0x00000420u, 0x80000017u, false)]
    [InlineData(0x00000420u, 0xC0000004u, false)]
    [InlineData(0x00000420u, 0x8000001Bu, false)]
    [InlineData(0x00000020u, 0xA0000003u, false)]
    public void Only_name_surrogate_reparse_points_redirect_path_resolution(
        uint fileAttributes,
        uint reparseTag,
        bool expected)
    {
        Assert.Equal(
            expected,
            RealIndexedShellTargetVerifier.RedirectsPathResolution(fileAttributes, reparseTag));
    }

    [Fact]
    public void A_junction_in_the_path_is_rejected_while_the_true_path_verifies()
    {
        var root = CreateNtfsFixtureDirectory("junction");
        var real = Path.Combine(root, "real");
        Directory.CreateDirectory(real);
        var file = Path.Combine(real, "target.txt");
        File.WriteAllText(file, "indexed");
        var junction = Path.Combine(root, "link");
        try
        {
            NativeJunction.Create(junction, real);
            ulong frn = RealIndexedShellTargetVerifier.ReadFileReference(file);

            RealIndexedShellTargetVerifier.Instance.VerifyAndPin(file, frn).Dispose();

            var ex = Assert.Throws<InvalidOperationException>(() =>
                RealIndexedShellTargetVerifier.Instance.VerifyAndPin(
                    Path.Combine(junction, "target.txt"),
                    frn));

            Assert.Equal(Loc.Get("Shell_ReparsePointBlocked"), ex.Message);
            Assert.NotEqual("Shell_ReparsePointBlocked", ex.Message);
        }
        finally
        {
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
        public void Restart(string arguments) =>
            throw new InvalidOperationException("restart failed");
    }

    [Fact]
    public void RelaunchWith_restarts_the_app_with_empty_arguments()
    {
        var restart = new RecordingRestart();

        ShellOps.RelaunchWith(restart);

        Assert.Equal(1, restart.Calls);
        Assert.Equal(string.Empty, restart.LastArguments);
    }

    [Fact]
    public void RelaunchWith_swallows_a_failed_restart_instead_of_throwing()
    {
        ShellOps.RelaunchWith(new ThrowingRestart());
    }

    [Fact]
    public void Hook_scope_rejects_null()
    {
        Assert.Throws<ArgumentNullException>(() => ShellOps.UseHooksForTests(null!));
    }

    [Fact]
    public void Shell_failures_preserve_user_messages_and_structured_operations()
    {
        using var log = new LogCapture();
        var notifications = new List<AppNotification>();
        using var subscription = Notifier.Attach(notifications.Add);

        ShellOps.OpenTrustedWith(new ThrowingRunner(), @"C:\dir\open.txt");
        ShellOps.OpenIndexedWith(
            new ThrowingVerifier(),
            new RecordingRunner(),
            @"C:\dir\indexed.txt",
            42);
        ShellOps.RelaunchWith(new ThrowingRestart());

        using (ShellOps.UseHooksForTests(Hooks(
            new RecordingRunner(),
            new ThrowingVerifier(),
            new RecordingRestart(),
            copyText: _ => throw new InvalidOperationException("clipboard busy"))))
        {
            ShellOps.RevealIndexed(@"C:\dir\reveal.txt", 43);
            ShellOps.CopyText("text", "diagnostics");
        }

        Assert.Contains(
            notifications,
            item => string.Equals(
                item.Message,
                $"{Loc.Get("Shell_OpenFailed")}: open.txt",
                StringComparison.Ordinal));
        Assert.Contains(
            notifications,
            item => string.Equals(
                item.Message,
                $"{Loc.Get("Shell_OpenFailed")}: indexed.txt",
                StringComparison.Ordinal));
        Assert.Contains(
            notifications,
            item => string.Equals(
                item.Message,
                $"{Loc.Get("Shell_RelaunchFailed")}: FindMyFiles",
                StringComparison.Ordinal));
        Assert.Contains(
            notifications,
            item => string.Equals(
                item.Message,
                $"{Loc.Get("Shell_RevealFailed")}: reveal.txt",
                StringComparison.Ordinal));
        Assert.Contains(
            notifications,
            item => string.Equals(
                    item.Message,
                    Loc.Get("Shell_ClipboardFailed"),
                    StringComparison.Ordinal)
                && item.Detail?.Contains("clipboard busy", StringComparison.Ordinal) == true);

        var lines = log.Text.Split('\n');
        foreach (var operation in new[] { "open", "open-indexed", "relaunch", "reveal" })
        {
            Assert.Contains(
                lines,
                line => line.Contains("area=shell", StringComparison.Ordinal)
                    && line.Contains("shell operation failed", StringComparison.Ordinal)
                    && line.Split(' ', StringSplitOptions.RemoveEmptyEntries)
                        .Contains($"operation={operation}", StringComparer.Ordinal));
        }

        Assert.Contains(
            lines,
            line => line.Contains("area=shell", StringComparison.Ordinal)
                && line.Contains("clipboard copy failed", StringComparison.Ordinal)
                && line.Contains("operation=diagnostics", StringComparison.Ordinal));
    }

    [Fact]
    public void Win32_not_found_switches_to_long_path_hint_only_after_max_path()
    {
        var error = new System.ComponentModel.Win32Exception(2);

        Assert.Equal(
            Loc.Get("Shell_HintMoved"),
            ShellOps.Hint(error, @"C:\" + new string('a', 256)));
        Assert.Equal(
            Loc.Get("Shell_HintPathTooLong"),
            ShellOps.Hint(error, @"C:\" + new string('a', 257)));
    }
}
