using Windows.ApplicationModel.DataTransfer;

namespace FindMyFiles.Services;

/// <summary>Complete set of shell/COM process boundaries used by
/// <see cref="ShellOps"/>. Production uses the operating system implementations;
/// tests replace the set atomically.</summary>
internal sealed record ShellOpsHooks(
    IProcessRunner ProcessRunner,
    IIndexedShellTargetVerifier Verifier,
    IRevealApi RevealApi,
    IAppRestart AppRestart,
    Action<Action> StartStaThread,
    Func<int> CoInitialize,
    Action CoUninitialize,
    Action<string> CopyText)
{
    /// <summary>The real Windows shell, COM, process, restart and clipboard boundaries.</summary>
    internal static ShellOpsHooks Production { get; } = new(
        RealProcessRunner.Instance,
        RealIndexedShellTargetVerifier.Instance,
        RealRevealApi.Instance,
        RealAppRestart.Instance,
        static action =>
        {
            var thread = new Thread(() => action()) { IsBackground = true };
            thread.SetApartmentState(ApartmentState.STA);
            thread.Start();
        },
        ShellOpsNative.CoInitialize,
        ShellOpsNative.CoUninitialize,
        static text =>
        {
            var package = new DataPackage();
            package.SetText(text);
            Clipboard.SetContent(package);
        });
}
