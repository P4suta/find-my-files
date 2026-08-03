namespace FindMyFiles.Services;

/// <summary>Process/elevation boundaries used by the production service
/// provisioner. Tests replace the complete set so exercising <see
/// cref="ServiceProvisioner.Real"/> never opens UAC or touches SCM state.</summary>
/// <param name="LocateServiceExe">Resolves the bundled helper.</param>
/// <param name="CreateSetupArguments">Builds the SID-bound setup command.</param>
/// <param name="RunElevated">Runs the closed command through the UAC boundary.</param>
internal sealed record ServiceProvisionerHooks(
    Func<string, string?> LocateServiceExe,
    Func<(bool Success, string Arguments)> CreateSetupArguments,
    Func<string, string, ServiceActionResult> RunElevated)
{
    /// <summary>The real filesystem, SID and elevation boundaries.</summary>
    internal static ServiceProvisionerHooks Production { get; } = new(
        ServiceSetup.LocateServiceExe,
        static () =>
        {
            var success = ServiceSetup.TryCreateSetupArguments(out var arguments);
            return (success, arguments);
        },
        ServiceSetup.RunElevated);
}
