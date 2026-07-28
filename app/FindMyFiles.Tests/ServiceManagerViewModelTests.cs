using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ServiceManagerViewModelTests
{
    [Fact]
    public void Unknown_state_offers_only_idempotent_repair()
    {
        var viewModel = new ServiceManagerViewModel(
            locateServiceExe: () => "fmf-service.exe");

        viewModel.ApplyState(EngineServiceState.Unknown);

        Assert.True(viewModel.IsUnknown);
        Assert.False(viewModel.IsInstalled);
        Assert.False(viewModel.IsRunning);
        Assert.False(viewModel.IsStopped);
        Assert.False(viewModel.IsNotInstalled);
        Assert.True(viewModel.CanRegister);
        Assert.True(viewModel.CanReregister);
        Assert.False(viewModel.CanStart);
        Assert.False(viewModel.CanStop);
        Assert.False(viewModel.CanRestart);
        Assert.False(viewModel.CanUninstall);
        Assert.False(viewModel.CanPurgeData);
        Assert.Equal(Loc.Get("Svc_StateUnavailable"), viewModel.StateText);
    }

    [Fact]
    public void Known_states_expose_only_their_valid_actions()
    {
        var viewModel = new ServiceManagerViewModel(
            locateServiceExe: () => "fmf-service.exe");

        viewModel.ApplyState(EngineServiceState.Stopped);
        Assert.True(viewModel.IsInstalled);
        Assert.True(viewModel.CanStart);
        Assert.True(viewModel.CanReregister);
        Assert.True(viewModel.CanUninstall);
        Assert.True(viewModel.CanPurgeData);
        Assert.False(viewModel.CanStop);
        Assert.False(viewModel.CanRestart);

        viewModel.ApplyState(EngineServiceState.Running);
        Assert.True(viewModel.IsInstalled);
        Assert.False(viewModel.CanStart);
        Assert.True(viewModel.CanStop);
        Assert.True(viewModel.CanRestart);
        Assert.True(viewModel.CanUninstall);
        Assert.True(viewModel.CanPurgeData);

        viewModel.ApplyState(EngineServiceState.NotInstalled);
        Assert.True(viewModel.IsNotInstalled);
        Assert.False(viewModel.IsInstalled);
        Assert.False(viewModel.CanReregister);
        Assert.False(viewModel.CanUninstall);
        Assert.True(viewModel.CanPurgeData);
    }

    [Fact]
    public void Lifecycle_control_does_not_require_the_elevated_helper_binary()
    {
        var viewModel = new ServiceManagerViewModel(
            queryState: () => EngineServiceState.Stopped,
            locateServiceExe: () => null);

        viewModel.Refresh();

        Assert.True(viewModel.IsStopped);
        Assert.True(viewModel.CanStart);
        Assert.False(viewModel.CanRegister);
        Assert.False(viewModel.CanReregister);
        Assert.False(viewModel.CanUninstall);
        Assert.False(viewModel.CanPurgeData);
    }

    [Fact]
    public void Purge_requires_a_known_state_but_not_an_installed_service()
    {
        var viewModel = new ServiceManagerViewModel(
            queryState: () => EngineServiceState.Running,
            locateServiceExe: () => "fmf-service.exe");

        viewModel.ApplyState(EngineServiceState.Unknown);
        viewModel.RequestPurgeConfirmation();
        Assert.False(viewModel.PurgeConfirmationVisible);

        viewModel.ApplyState(EngineServiceState.NotInstalled);
        viewModel.RequestPurgeConfirmation();
        Assert.True(viewModel.PurgeConfirmationVisible);

        viewModel.CancelPurgeConfirmation();
        viewModel.ApplyState(EngineServiceState.Running);
        viewModel.RequestPurgeConfirmation();
        Assert.True(viewModel.PurgeConfirmationVisible);

        viewModel.CancelPurgeConfirmation();
        Assert.False(viewModel.PurgeConfirmationVisible);
    }

    [Fact]
    public async Task Service_only_uninstall_is_rejected_after_service_is_gone()
    {
        var uninstallCalls = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => { },
            uninstall: _ =>
            {
                uninstallCalls++;
                return Task.FromResult(
                    new ServiceActionResult(ServiceActionOutcome.Ok, 0));
            });
        var viewModel = new ServiceManagerViewModel(
            provisioner,
            queryState: () => EngineServiceState.NotInstalled,
            locateServiceExe: () => "fmf-service.exe");
        viewModel.ApplyState(EngineServiceState.NotInstalled);

        await viewModel.UninstallAsync(purgeData: false);

        Assert.Equal(0, uninstallCalls);
        Assert.False(viewModel.Busy);
    }

    [Fact]
    public async Task Confirmed_full_purge_exits_without_soft_restart_on_success()
    {
        bool? purgeRequested = null;
        var restarts = 0;
        var exits = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => { },
            uninstall: purge =>
            {
                purgeRequested = purge;
                return Task.FromResult(
                    new ServiceActionResult(ServiceActionOutcome.Ok, 0));
            },
            purgeUserData: () => true);
        var viewModel = new ServiceManagerViewModel(
            provisioner,
            restartApp: () => restarts++,
            exitApp: () => exits++,
            queryState: () => EngineServiceState.NotInstalled,
            locateServiceExe: () => "fmf-service.exe");
        viewModel.ApplyState(EngineServiceState.Running);
        viewModel.RequestPurgeConfirmation();

        await viewModel.UninstallAsync(purgeData: true);

        Assert.True(purgeRequested is true);
        Assert.Equal(0, restarts);
        Assert.Equal(1, exits);
        Assert.True(viewModel.FullUninstallCompleted);
        Assert.False(viewModel.PurgeConfirmationVisible);
        Assert.False(viewModel.Busy);
        Assert.Equal(Loc.Get("Svc_UninstalledWithData"), viewModel.ResultText);
    }

    [Theory]
    [InlineData((int)ServiceActionOutcome.Cancelled)]
    [InlineData((int)ServiceActionOutcome.Failed)]
    public async Task Failed_or_cancelled_purge_never_restarts(int outcomeValue)
    {
        var restarts = 0;
        var exits = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => { },
            uninstall: _ => Task.FromResult(
                new ServiceActionResult((ServiceActionOutcome)outcomeValue, -1)));
        var viewModel = new ServiceManagerViewModel(
            provisioner,
            restartApp: () => restarts++,
            exitApp: () => exits++,
            queryState: () => EngineServiceState.Running,
            locateServiceExe: () => "fmf-service.exe");
        viewModel.ApplyState(EngineServiceState.Running);

        await viewModel.UninstallAsync(purgeData: true);

        Assert.Equal(0, restarts);
        Assert.Equal(0, exits);
        Assert.False(viewModel.Busy);
        Assert.NotEmpty(viewModel.ResultText);
    }

    [Fact]
    public async Task App_data_purge_failure_is_visible_and_keeps_the_app_open()
    {
        var exits = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => { },
            uninstall: _ => Task.FromResult(
                new ServiceActionResult(ServiceActionOutcome.Ok, 0)),
            purgeUserData: () => false);
        var viewModel = new ServiceManagerViewModel(
            provisioner,
            restartApp: () => { },
            exitApp: () => exits++,
            queryState: () => EngineServiceState.NotInstalled,
            locateServiceExe: () => "fmf-service.exe");
        viewModel.ApplyState(EngineServiceState.Running);

        await viewModel.UninstallAsync(purgeData: true);

        Assert.Equal(0, exits);
        Assert.Equal(NotifySeverity.Error, viewModel.ResultSeverity);
        Assert.Equal(Loc.Get("Svc_UserDataPurgeFailed"), viewModel.ResultText);
        Assert.True(viewModel.IsNotInstalled);
        Assert.False(viewModel.CanUninstall);
        Assert.True(viewModel.CanPurgeData);

        viewModel.RequestPurgeConfirmation();
        Assert.True(viewModel.PurgeConfirmationVisible);
    }

    [Fact]
    public async Task Full_purge_can_be_retried_after_user_data_failure_when_service_is_gone()
    {
        var uninstallCalls = 0;
        var purgeCalls = 0;
        var exits = 0;
        var provisioner = new ServiceProvisioner(
            register: () => Task.FromResult(ServiceActionOutcome.Ok),
            relaunch: () => { },
            uninstall: purge =>
            {
                Assert.True(purge);
                uninstallCalls++;
                return Task.FromResult(
                    new ServiceActionResult(ServiceActionOutcome.Ok, 0));
            },
            purgeUserData: () => ++purgeCalls == 2);
        var viewModel = new ServiceManagerViewModel(
            provisioner,
            exitApp: () => exits++,
            queryState: () => EngineServiceState.NotInstalled,
            locateServiceExe: () => "fmf-service.exe");
        viewModel.ApplyState(EngineServiceState.Running);

        await viewModel.UninstallAsync(purgeData: true);
        viewModel.RequestPurgeConfirmation();
        await viewModel.UninstallAsync(purgeData: true);

        Assert.Equal(2, uninstallCalls);
        Assert.Equal(2, purgeCalls);
        Assert.Equal(1, exits);
        Assert.True(viewModel.FullUninstallCompleted);
        Assert.Equal(Loc.Get("Svc_UninstalledWithData"), viewModel.ResultText);
    }
}
