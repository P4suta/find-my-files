using System.Runtime.InteropServices;

namespace FindMyFiles.Services;

/// <summary>Result of one <see cref="ServiceSetup.RunElevated"/> call: the
/// classified <paramref name="Outcome"/> plus the raw process
/// <paramref name="ExitCode"/> (-1 when the process never produced one).</summary>
/// <param name="Outcome">Success / failure / user-cancelled classification.</param>
/// <param name="ExitCode">fmf-service.exe exit code, or -1 if it could not be
/// launched, timed out, or the UAC prompt was declined.</param>
[StructLayout(LayoutKind.Auto)]
public readonly record struct ServiceActionResult(ServiceActionOutcome Outcome, int ExitCode);
