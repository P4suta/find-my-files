<#
.SYNOPSIS
    winapp-UI-automation smoke suite for FindMyFiles (the WinUI 3 app).

.DESCRIPTION
    Scripted batch UI test. Drives the PUBLISHED FindMyFiles.exe through the
    `winapp ui` automation verbs (wait-for / invoke / set-value / click /
    screenshot) and asserts on AutomationIds declared in app/FindMyFiles/
    MainPage.xaml. Two launch modes are exercised:

      --engine=unavailable  forces the disconnected first-run setup screen
                      (Slice A; FakeEngineClient.CreateEmpty, IsDisconnected=true).
      --fake-engine   loads deterministic 100k-row data (seed 42) so search,
                      sort, option-toggle and fault-injection scenarios are
                      reproducible without touching a real volume.

    DEBUG --fake-engine also honours the fault queries !!panic / !!lag / !!warn
    (FakeEngineClient.SearchAsync) so the InfoBar/NotifyBar error pipeline can be
    verified end-to-end. Those scenarios are guarded by -IncludeFaults because
    they only fire in a DEBUG build of the app.

    This script does NOT build or publish — the `just ui-test` recipe publishes
    the bundle and launches the exe, then passes us the PID. To run standalone,
    launch the exe yourself and pass -AppPid, or pass -ExePath to let the script
    launch it under --fake-engine.

.NOTES
    The `winapp ui` CLI is the project's UI automation harness. If a primitive is
    unavailable on this machine the per-test try/catch records a FAIL with the
    underlying error rather than aborting the run. Verb reference:
        winapp ui --cli-schema
        winapp ui <verb> --help
#>
[CmdletBinding(DefaultParameterSetName = 'Pid')]
param(
    # PID of an already-running FindMyFiles.exe (preferred — the recipe launches
    # it under --fake-engine and passes the PID). NOTE: never name this $Pid —
    # $Pid is a read-only automatic variable in PowerShell.
    [Parameter(Mandatory, ParameterSetName = 'Pid')]
    [int]$AppPid,

    # Path to the published FindMyFiles.exe; the script launches it itself. Used
    # for the --engine=unavailable setup-screen phase, which needs its own process.
    [Parameter(Mandatory, ParameterSetName = 'Exe')]
    [string]$ExePath,

    # Run the DEBUG-only fault-injection phase (!!panic / !!lag). Skipped by
    # default because a Release bundle compiles those branches out.
    [switch]$IncludeFaults,

    # Exercise the actual shipping binary with no test-only command-line seams.
    # This is intentionally a tiny launch/UIA smoke; deterministic interaction
    # coverage runs against the separately compiled test-seam bundle.
    [switch]$StableSmoke,

    # Where screenshots + the results JSON land. Resolve from the script so a
    # standalone invocation still obeys ADR-0021 regardless of the current dir.
    [string]$OutDir = (Join-Path $PSScriptRoot '..\..\..\build\ui-automation')
)

$ErrorActionPreference = 'Continue'
$script:pass = 0
$script:fail = 0
$script:results = @()

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$script:OutRoot = (Resolve-Path -LiteralPath $OutDir).Path
$script:DataDir = Join-Path $script:OutRoot ("state-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $script:DataDir | Out-Null
$script:LogSource = Join-Path $script:DataDir 'logs'

# `get-focused` is a global UIA query: it cannot report an element in a window
# that never became foreground. Test runners commonly launch pwsh from a
# different foreground process, so ordinary SetForegroundWindow is rejected by
# Windows' foreground lock. Join only the caller, current foreground, and exact
# target HWND input queues for the activation operation, then detach every join
# in a finally block. No child element receives focus here: the assertion still
# verifies the app's production title-bar-to-primary-action handoff.
if (-not ('FindMyFiles.UiTests.NativeWindow' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace FindMyFiles.UiTests
{
    public static class NativeWindow
    {
        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr GetForegroundWindow();

        [DllImport("user32.dll", SetLastError = true)]
        public static extern uint GetWindowThreadProcessId(
            IntPtr window,
            out uint processId);

        [DllImport("kernel32.dll")]
        public static extern uint GetCurrentThreadId();

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool AttachThreadInput(
            uint attachThread,
            uint attachToThread,
            bool attach);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool ShowWindowAsync(IntPtr window, int command);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool BringWindowToTop(IntPtr window);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool SetForegroundWindow(IntPtr window);
    }
}
'@
}

function Activate-AppWindow {
    param(
        [Parameter(Mandatory)]
        [System.Diagnostics.Process]$Process,
        [int]$TimeoutMs = 5000
    )

    $processId = $Process.Id
    $windowDeadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    $window = [IntPtr]::Zero
    do {
        if ($Process.HasExited) {
            throw "process $processId exited before its main window was created"
        }
        $Process.Refresh()
        $window = $Process.MainWindowHandle
        if ($window -ne [IntPtr]::Zero) {
            break
        }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $windowDeadline)

    if ($window -eq [IntPtr]::Zero) {
        throw "process $processId did not publish a main window within ${TimeoutMs}ms"
    }

    $windowProcessId = [uint32]0
    $targetThread = [FindMyFiles.UiTests.NativeWindow]::GetWindowThreadProcessId(
        $window,
        [ref]$windowProcessId)
    if ($targetThread -eq 0 -or $windowProcessId -ne $processId) {
        throw "main HWND $window is not owned by process $processId"
    }

    $callerThread = [FindMyFiles.UiTests.NativeWindow]::GetCurrentThreadId()
    $foregroundWindow = [FindMyFiles.UiTests.NativeWindow]::GetForegroundWindow()
    $foregroundProcessId = [uint32]0
    $foregroundThread = if ($foregroundWindow -eq [IntPtr]::Zero) {
        [uint32]0
    }
    else {
        [FindMyFiles.UiTests.NativeWindow]::GetWindowThreadProcessId(
            $foregroundWindow,
            [ref]$foregroundProcessId)
    }

    $threadsToAttach = @($foregroundThread, $targetThread) |
        Where-Object { $_ -ne 0 -and $_ -ne $callerThread } |
        Select-Object -Unique
    $attachedThreads = [System.Collections.Generic.List[uint32]]::new()

    try {
        foreach ($thread in $threadsToAttach) {
            if (-not [FindMyFiles.UiTests.NativeWindow]::AttachThreadInput(
                $callerThread,
                $thread,
                $true)) {
                $nativeError =
                    [Runtime.InteropServices.Marshal]::GetLastWin32Error()
                throw "could not attach to input thread $thread (Win32 $nativeError)"
            }
            $attachedThreads.Add($thread)
        }

        # Revalidate the HWND ownership immediately before changing foreground
        # state. This closes the exit/PID/HWND reuse window after the first check.
        if ($Process.HasExited) {
            throw "process $processId exited before activation"
        }
        $confirmedProcessId = [uint32]0
        $confirmedThread =
            [FindMyFiles.UiTests.NativeWindow]::GetWindowThreadProcessId(
                $window,
                [ref]$confirmedProcessId)
        if (($confirmedThread -ne $targetThread) -or
            ($confirmedProcessId -ne $processId)) {
            throw "main HWND $window changed ownership before activation"
        }

        # SW_RESTORE (9) is safe for normal and minimized windows. Its return
        # value describes prior visibility, so only the foreground operations
        # below are success predicates.
        [void][FindMyFiles.UiTests.NativeWindow]::ShowWindowAsync($window, 9)
        $broughtToTop =
            [FindMyFiles.UiTests.NativeWindow]::BringWindowToTop($window)
        $madeForeground =
            [FindMyFiles.UiTests.NativeWindow]::SetForegroundWindow($window)
        if (-not $broughtToTop -or -not $madeForeground) {
            $nativeError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
            throw "Windows rejected activation for HWND $window (Win32 $nativeError)"
        }
    }
    finally {
        for ($index = $attachedThreads.Count - 1; $index -ge 0; $index--) {
            [void][FindMyFiles.UiTests.NativeWindow]::AttachThreadInput(
                $callerThread,
                $attachedThreads[$index],
                $false)
        }
    }

    $activationDeadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    do {
        if ([FindMyFiles.UiTests.NativeWindow]::GetForegroundWindow() -eq $window) {
            return
        }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $activationDeadline)

    throw "HWND $window for process $processId did not become foreground within ${TimeoutMs}ms"
}

# ── Harness shim ──────────────────────────────────────────────────────────────
# Single chokepoint for the UI automation CLI. Every scenario goes through here,
# so if this machine's harness binary is named/invoked differently, this is the
# ONE place to adapt. CI passes the verified runner-temp executable explicitly;
# local development resolves the mise-provisioned binary from PATH.
#
$script:UiCli = if ($env:FMF_UI_CLI) {
    $env:FMF_UI_CLI
}
else {
    'winapp'
}
if (-not (Get-Command -Name $script:UiCli -ErrorAction SilentlyContinue)) {
    throw "UI automation harness '$script:UiCli' is unavailable; run `just doctor`."
}

function Invoke-Ui {
    # Simple (non-advanced) function on purpose. A [Parameter()] block would make
    # this an advanced function with the PowerShell common parameters, which then
    # intercept the verbs' own flags: `-a`/`-o`/`-p` partial-match -OutVariable /
    # -ProgressAction / -OutBuffer and never reach the CLI ("a positional parameter
    # cannot be found…"). The automatic $args captures EVERY token — dash-flags
    # included — and splats them verbatim to the winapp CLI.
    & $script:UiCli ui @args
}

function Test-UI {
    param([string]$Name, [scriptblock]$Script)
    # Inside $Script use `throw` to fail a single test — NOT `exit`, which would
    # terminate the whole suite. A non-zero $LASTEXITCODE from the CLI is a fail.
    try {
        # A pure PowerShell assertion does not update LASTEXITCODE. Reset it so
        # a failed CLI probe cannot poison the following in-process check.
        $global:LASTEXITCODE = 0
        $output = & $Script 2>&1
        if ($LASTEXITCODE -eq 0) {
            $script:pass++
            $script:results += @{ name = $Name; status = 'PASS' }
            Write-Host "  PASS: $Name" -ForegroundColor Green
        } else {
            $script:fail++
            $script:results += @{ name = $Name; status = 'FAIL'; detail = "$output" }
            Write-Host "  FAIL: $Name — $output" -ForegroundColor Red
        }
    } catch {
        $script:fail++
        $script:results += @{ name = $Name; status = 'FAIL'; detail = "$_" }
        Write-Host "  FAIL: $Name — $_" -ForegroundColor Red
    }
}

function Assert-AccessibleElement {
    param(
        [string]$Selector,
        [int]$ProcessId,
        [string]$WindowHandle
    )
    $cliArgs = @('get-property', $Selector, '--json')
    if ($WindowHandle) {
        $cliArgs += @('-w', $WindowHandle)
    } else {
        $cliArgs += @('-a', $ProcessId)
    }
    $element = Invoke-Ui @cliArgs 2>$null | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $null -eq $element) {
        throw "'$Selector' was not readable through UI Automation"
    }
    if ([string]::IsNullOrWhiteSpace($element.properties.AutomationId)) {
        throw "'$Selector' has no AutomationId"
    }
    if ([string]::IsNullOrWhiteSpace($element.properties.Name)) {
        throw "'$Selector' has no accessible Name"
    }
}

# Launch the published exe with the given args and return its PID. Used for the
# setup-screen phase, which needs its own --engine=unavailable process.
function Start-App {
    param([string]$Exe, [string[]]$AppArgs)
    if (-not (Test-Path $Exe)) {
        throw "FindMyFiles.exe not found at '$Exe' — run `just publish` first."
    }
    # Every launched process receives a unique test-owned state root. This keeps
    # local settings and a previously published portable bundle from changing
    # defaults or counts, and prevents the suite from touching the user's profile.
    $arguments = @($AppArgs) + "--data-dir=$script:DataDir"
    $p = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru
    Activate-AppWindow -Process $p
    # Give the WinUI window + the automation tree time to materialise. The first
    # wait-for in each phase has its own timeout, so this is just startup slack.
    Start-Sleep -Seconds 2
    return $p.Id
}

# Launch the real shipping artifact without --fake-engine, --engine=unavailable,
# --data-dir, or a custom pipe. APPDATA/LOCALAPPDATA are process-local test
# isolation, not application command-line seams, and leave the user's profile
# untouched.
function Start-StableApp {
    param([string]$Exe)
    if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
        throw "Shipping FindMyFiles.exe not found at '$Exe' — run `just publish` first."
    }
    $localData = Join-Path $script:DataDir 'local'
    New-Item -ItemType Directory -Force -Path $localData | Out-Null
    $script:LogSource = Join-Path $script:DataDir 'find-my-files\logs'
    $process = Start-Process -FilePath $Exe -PassThru -Environment @{
        APPDATA = $script:DataDir
        LOCALAPPDATA = $localData
    }
    Activate-AppWindow -Process $process
    Start-Sleep -Seconds 2
    return $process.Id
}

# Tear an app instance down WITHOUT leaving a DWM ghost window. A bare
# `Stop-Process -Force` kills the process while its top-level window is still
# mapped, so the shell keeps a phantom Alt+Tab entry with no process behind it
# (the user cannot dismiss it). CloseMainWindow posts WM_CLOSE so WinUI runs its
# teardown and unmaps / tray-hides the window first; -Force is the fallback for a
# window that does not honour the close in time (e.g. a modal dialog still up).
function Stop-AppGracefully {
    param([int]$ProcId)
    if (-not $ProcId) { return }
    $proc = Get-Process -Id $ProcId -ErrorAction SilentlyContinue
    if (-not $proc) { return }
    try { $proc.CloseMainWindow() | Out-Null } catch { }
    if (-not $proc.WaitForExit(2000)) {
        Stop-Process -Id $ProcId -Force -ErrorAction SilentlyContinue
    }
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase A — first-run SETUP screen under --engine=unavailable
#   IsDisconnected=true collapses the search UI (IsReady=false) and centres the
#   setup StackPanel. The service registration button is deliberately the only
#   production ingest path; the former no-admin directory-walk CTA must stay gone.
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-SetupPhase {
    param([string]$Exe)
    Write-Host "`n=== Phase A: first-run setup (--engine=unavailable) ===" -ForegroundColor Cyan

    $setupPid = $null
    try {
        $setupPid = Start-App -Exe $Exe -AppArgs @('--engine=unavailable')
    } catch {
        $script:fail++
        $script:results += @{ name = 'Setup: launch --engine=unavailable'; status = 'FAIL'; detail = "$_" }
        Write-Host "  FAIL: Setup launch — $_" -ForegroundColor Red
        return
    }

    Test-UI 'Setup: EnableSearch button present' {
        Invoke-Ui wait-for 'EnableSearch' -a $setupPid -t 5000
    }
    Test-UI 'Setup: legacy directory-scan fallback absent' {
        Invoke-Ui wait-for 'ScopeSetupLink' -a $setupPid --gone -t 3000
    }
    Test-UI 'Setup: EnableSearch is enabled (SetupNotBusy)' {
        Invoke-Ui wait-for 'EnableSearch' -a $setupPid -p IsEnabled --value 'True' -t 3000
    }
    Test-UI 'Setup: primary action has accessible identity and name' {
        Assert-AccessibleElement -Selector 'EnableSearch' -ProcessId $setupPid
        $global:LASTEXITCODE = 0
    }
    Test-UI 'Setup: recovery action is present and enabled' {
        Invoke-Ui wait-for 'SetupRecovery' -a $setupPid -p IsEnabled --value 'True' -t 3000
    }
    Test-UI 'Setup: recovery action has accessible identity and name' {
        Assert-AccessibleElement -Selector 'SetupRecovery' -ProcessId $setupPid
        $global:LASTEXITCODE = 0
    }
    # Search UI is collapsed on the setup screen (IsReady=false): SearchBox must
    # NOT be interactable. wait-for --gone is the disconnected-state invariant.
    Test-UI 'Setup: SearchBox collapsed while disconnected' {
        Invoke-Ui wait-for 'SearchBox' -a $setupPid --gone -t 3000
    }
    Test-UI 'Setup: focus moves to the visible primary recovery action' {
        $focused = Invoke-Ui get-focused -a $setupPid --json 2>$null | ConvertFrom-Json
        if ($focused.element.automationId -ne 'EnableSearch') {
            throw "focus remained on '$($focused.element.automationId)'"
        }
    }
    Invoke-Ui screenshot -a $setupPid -o (Join-Path $OutDir 'A-setup.png') 2>$null

    Test-UI 'Setup: recovery opens settings while disconnected' {
        Invoke-Ui invoke 'SetupRecovery' -a $setupPid | Out-Null
        Invoke-Ui wait-for 'SettingsDialog' -a $setupPid -t 3000
    }
    Test-UI 'Setup: diagnostics and service management stay published' {
        Assert-AccessibleElement -Selector 'DiagToggle' -ProcessId $setupPid
        Assert-AccessibleElement -Selector 'ServiceManageMenu' -ProcessId $setupPid
        $global:LASTEXITCODE = 0
    }
    Invoke-Ui screenshot -a $setupPid -o (Join-Path $OutDir 'A-recovery-settings.png') 2>$null

    Test-UI 'Setup: diagnostics is reachable' {
        Invoke-Ui invoke 'DiagToggle' -a $setupPid | Out-Null
        Invoke-Ui wait-for 'PerfPanel' -a $setupPid -t 5000
    }
    Test-UI 'Setup: service manager is reachable' {
        Invoke-Ui invoke 'SetupRecovery' -a $setupPid | Out-Null
        Invoke-Ui wait-for 'SettingsDialog' -a $setupPid -t 3000 | Out-Null
        Invoke-Ui invoke 'ServiceManageMenu' -a $setupPid | Out-Null
        Invoke-Ui wait-for 'ServiceManagerDialog' -a $setupPid -t 5000
    }
    Test-UI 'Setup: full cleanup retry is reachable while unregistered' {
        Assert-AccessibleElement -Selector 'SvcPurgeData' -ProcessId $setupPid
        $global:LASTEXITCODE = 0
    }
    Invoke-Ui screenshot -a $setupPid -o (Join-Path $OutDir 'A-recovery-service.png') 2>$null
    Invoke-Ui invoke 'CloseButton' -a $setupPid 2>$null | Out-Null

    Stop-AppGracefully $setupPid
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase B — SEARCH interactions under --fake-engine (deterministic 100k rows)
#   Data shape (FakeEngineClient, seed 42): names file_NNNNNN_x.ext; folders
#   folder_NNNNNN every 50th row; hidden/system rows hidden_sys_NNNNNN.dat every
#   97th row. Settings/status now live in a modal ContentDialog opened from the
#   gear; toggles, sort and StatusCount are read inside it, and it must be closed
#   before touching the page (SearchBox / ResultsList) underneath.
# ──────────────────────────────────────────────────────────────────────────────
function Open-Settings {
    Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
    Invoke-Ui wait-for 'SettingsDialog' -a $AppPid -t 3000 | Out-Null
}

function Close-Settings {
    # WinUI names the ContentDialog's close button 'CloseButton' (template part),
    # which surfaces as that AutomationId — language-independent, unlike its text.
    Invoke-Ui invoke 'CloseButton' -a $AppPid 2>$null | Out-Null
    Start-Sleep -Milliseconds 200
}

function Get-StatusCountText {
    # StatusCount lives inside the settings dialog now; open, read, close.
    Open-Settings
    $v = Invoke-Ui get-value 'StatusCount' -a $AppPid --json 2>$null | ConvertFrom-Json
    Close-Settings
    return $v.text
}

function Get-ResultRow {
    param([int]$Index)
    $result = Invoke-Ui get-property "ResultRow-$Index" -a $AppPid --json 2>$null |
        ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $null -eq $result) {
        throw "ResultRow-$Index was not readable through UI Automation"
    }
    return $result
}

function Get-VisibleResultRows {
    $result = Invoke-Ui search 'ResultRow-' -a $AppPid --json --max 100 2>$null |
        ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $null -eq $result) {
        throw 'result rows were not searchable through UI Automation'
    }
    return @($result.matches | Where-Object { -not $_.isOffscreen })
}

function Wait-ResultRowNameChange {
    param([int]$Index, [string]$Previous, [int]$TimeoutMs = 5000)
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    do {
        $name = (Get-ResultRow -Index $Index).properties.Name
        if (-not [string]::IsNullOrWhiteSpace($name) -and $name -ne $Previous) {
            return $name
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "ResultRow-$Index did not change from '$Previous' within ${TimeoutMs}ms"
}

function Invoke-SearchPhase {
    Write-Host "`n=== Phase B: search interactions (--fake-engine) ===" -ForegroundColor Cyan

    Test-UI 'Search: SearchBox present (engine ready)' {
        Invoke-Ui wait-for 'SearchBox' -a $AppPid -t 5000
    }
    Test-UI 'Search: ResultsList present' {
        Invoke-Ui wait-for 'ResultsList' -a $AppPid -t 3000
    }
    Test-UI 'Search: OptionsButton present' {
        Invoke-Ui wait-for 'OptionsButton' -a $AppPid -t 3000
    }
    Test-UI 'Accessibility: main interactive surfaces are named' {
        foreach ($selector in @('SearchBox', 'OptionsButton', 'ResultsList')) {
            Assert-AccessibleElement -Selector $selector -ProcessId $AppPid
        }
        $global:LASTEXITCODE = 0
    }

    # Type a needle that matches a deterministic subset. SearchText binds with
    # UpdateSourceTrigger=PropertyChanged, so set-value drives the live filter
    # without needing a LostFocus commit.
    Test-UI 'Search: type "file_0" into SearchBox' {
        Invoke-Ui set-value 'SearchBox' 'file_0' -a $AppPid
    }
    Test-UI 'Search: SearchBox holds the typed text' {
        Invoke-Ui wait-for 'SearchBox' -a $AppPid --value 'file_0' -t 2000
    }
    Test-UI 'Search: first result row is filled and visible to UIA' {
        Invoke-Ui wait-for 'ResultRow-0' -a $AppPid --value 'file_0' --contains -t 5000
    }
    Test-UI 'Search: result row exposes stable accessible semantics' {
        $row = Get-ResultRow -Index 0
        $properties = $row.properties
        if ($properties.AutomationId -ne 'ResultRow-0') {
            throw "unexpected row AutomationId '$($properties.AutomationId)'"
        }
        if ($properties.ControlType -ne 'ListItem' -or $properties.ClassName -ne 'ListViewItem') {
            throw "row is not a ListViewItem ($($properties.ControlType)/$($properties.ClassName))"
        }
        if ($properties.Name -notlike '*file_0*') {
            throw "row has no populated accessible name ('$($properties.Name)')"
        }
        if ($properties.IsEnabled -ne 'True' -or
            $properties.IsOffscreen -ne 'False' -or
            $properties.IsKeyboardFocusable -ne 'True') {
            throw "row is not enabled, visible, and keyboard-focusable: $($properties | ConvertTo-Json -Compress)"
        }
        $global:LASTEXITCODE = 0
    }
    Test-UI 'Search: result row accepts keyboard focus' {
        Invoke-Ui focus 'ResultRow-0' -a $AppPid | Out-Null
        $focused = Invoke-Ui get-focused -a $AppPid --json 2>$null | ConvertFrom-Json
        if ($focused.element.automationId -ne 'ResultRow-0') {
            throw "focus remained on '$($focused.element.automationId)'"
        }
        $global:LASTEXITCODE = 0
    }

    # StatusCount is localized, so the row assertions above prove non-zero data;
    # here we only require that the status surface itself is populated.
    Test-UI 'Search: StatusCount reflects the query (non-empty)' {
        $countText = Get-StatusCountText
        if ([string]::IsNullOrWhiteSpace($countText)) {
            throw 'StatusCount text was empty after typing a query'
        }
        $global:LASTEXITCODE = 0
    }

    # Capture the open settings dialog for a visual check of the SettingsCard surface.
    Open-Settings
    Test-UI 'Accessibility: settings controls have identities and names' {
        foreach ($selector in @(
            'OptFocused',
            'OptSystem',
            'OptRegex',
            'RegexScopeName',
            'RegexScopePath',
            'SortName',
            'SortSize',
            'SortDate',
            'SortDescending',
            'LangCombo',
            'OptCloseToTray',
            'DiagToggle',
            'ServiceManageMenu')) {
            Assert-AccessibleElement -Selector $selector -ProcessId $AppPid
        }
        $global:LASTEXITCODE = 0
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'B-settings.png') 2>$null
    Close-Settings

    # ── Sort reorder: SortName / SortSize / SortDate are RadioButtons in the
    #    settings dialog's sort card. Selecting each must succeed and leave
    #    ResultsList intact (the virtualized list must not blank out). Each opens
    #    the dialog, selects, then closes — the page is modal-blocked while it is up.
    function Invoke-Sort {
        param([string]$SortId)
        Open-Settings
        Invoke-Ui invoke $SortId -a $AppPid
        if ($LASTEXITCODE -eq 0) {
            Invoke-Ui wait-for $SortId -a $AppPid -p IsSelected --value 'True' -t 2000 | Out-Null
        }
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    Test-UI 'Sort: SortName applies' { Invoke-Sort 'SortName' }
    $script:nameSortedFirst = (Get-ResultRow -Index 0).properties.Name
    Test-UI 'Sort: SortSize changes the first result' {
        Invoke-Sort 'SortSize'
        if ($LASTEXITCODE -ne 0) { return }
        $script:sizeSortedFirst = Wait-ResultRowNameChange -Index 0 -Previous $script:nameSortedFirst
        $global:LASTEXITCODE = 0
    }
    Test-UI 'Sort: SortDate changes the first result again' {
        Invoke-Sort 'SortDate'
        if ($LASTEXITCODE -ne 0) { return }
        $null = Wait-ResultRowNameChange -Index 0 -Previous $script:sizeSortedFirst
        $global:LASTEXITCODE = 0
    }

    # ── OptRegex toggle: switches the fake into .NET-regex filtering. The needle
    #    "file_0" is a valid regex, so results stay non-empty; the ToggleSwitch
    #    itself must flip to On. (Capture the action's exit code before the close,
    #    so Test-UI judges the toggle, not the close.)
    Test-UI 'OptRegex: toggle on' {
        Open-Settings
        Invoke-Ui invoke 'OptRegex' -a $AppPid
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    Test-UI 'OptRegex: reads On' {
        Open-Settings
        Invoke-Ui wait-for 'OptRegex' -a $AppPid --value 'On' -t 2000
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    # Toggle regex back off so the system-files assertion below filters by plain
    # substring (deterministic count delta).
    Test-UI 'OptRegex: toggle back off' {
        Open-Settings
        Invoke-Ui invoke 'OptRegex' -a $AppPid
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }

    # ── OptSystem toggle: focused search intentionally rejects .dat, so disable
    #    it first. hidden_sys_* then proves the hidden/system predicate itself.
    Test-UI 'OptFocused: toggle off for the system-file scenario' {
        Open-Settings
        Invoke-Ui invoke 'OptFocused' -a $AppPid
        if ($LASTEXITCODE -eq 0) {
            Invoke-Ui wait-for 'OptFocused' -a $AppPid --value 'Off' -t 2000 | Out-Null
        }
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    Test-UI 'OptSystem: search a hidden/system-only needle' {
        Invoke-Ui set-value 'SearchBox' 'hidden_sys' -a $AppPid
    }
    Test-UI 'OptSystem: system rows are absent while off' {
        Invoke-Ui wait-for 'ResultRow-0' -a $AppPid --gone -t 5000
        if ($LASTEXITCODE -eq 0) {
            Invoke-Ui wait-for 'NoResultsTitle' -a $AppPid -t 3000
        }
    }
    Test-UI 'OptSystem: toggle on' {
        Open-Settings
        Invoke-Ui invoke 'OptSystem' -a $AppPid
        if ($LASTEXITCODE -eq 0) {
            Invoke-Ui wait-for 'OptSystem' -a $AppPid --value 'On' -t 2000 | Out-Null
        }
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    Test-UI 'OptSystem: system rows appear with populated names' {
        Invoke-Ui wait-for 'ResultRow-0' -a $AppPid --value 'hidden_sys' --contains -t 5000
    }
    # Reset every option changed by this phase.
    Open-Settings
    Invoke-Ui invoke 'OptSystem' -a $AppPid 2>$null | Out-Null
    Invoke-Ui invoke 'OptFocused' -a $AppPid 2>$null | Out-Null
    Close-Settings
    Invoke-Ui set-value 'SearchBox' '' -a $AppPid 2>$null | Out-Null
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'B-search.png') 2>$null
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase B2 — service-management surface (read-only)
#   Opens the dialog and verifies its state/action wiring without invoking any
#   lifecycle mutation or UAC prompt. The visible action depends on local SCM
#   state, so accept Register (absent) or Re-register (installed/unreadable).
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-ServiceSurfacePhase {
    Write-Host "`n=== Phase B2: service-management surface (read-only) ===" -ForegroundColor Cyan

    Test-UI 'Service UI: open from settings' {
        Open-Settings
        Invoke-Ui invoke 'ServiceManageMenu' -a $AppPid | Out-Null
        Invoke-Ui wait-for 'ServiceManagerDialog' -a $AppPid -t 5000
    }
    Test-UI 'Service UI: state is populated' {
        $state = Invoke-Ui get-value 'SvcState' -a $AppPid --json 2>$null | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($state.text)) {
            throw 'service state text was empty'
        }
        $global:LASTEXITCODE = 0
    }
    Test-UI 'Service UI: visible repair action is accessible' {
        $found = $false
        foreach ($selector in @('SvcRegister', 'SvcReregister')) {
            Invoke-Ui wait-for $selector -a $AppPid -t 500 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) {
                Assert-AccessibleElement -Selector $selector -ProcessId $AppPid
                $found = $true
                break
            }
        }
        if (-not $found) {
            throw 'neither Register nor Re-register was visible'
        }
        $global:LASTEXITCODE = 0
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'B-service.png') 2>$null
    Invoke-Ui invoke 'CloseButton' -a $AppPid 2>$null | Out-Null
    Start-Sleep -Milliseconds 200
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase C — virtualized scroll fetch (no blank rows)
#   The data-virtualized ListView (ItemsStackPanel + IItemsRangeInfo) fetches
#   pages on demand. Scrolling deep then asserting a far row has real text proves
#   the fetch path fills cells instead of leaving placeholders blank (ADR-0015).
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-ScrollPhase {
    Write-Host "`n=== Phase C: virtualized scroll fetch ===" -ForegroundColor Cyan

    # Broad needle → tens of thousands of hits, enough to scroll past several
    # virtualization pages.
    Test-UI 'Scroll: broad query populates the list' {
        Invoke-Ui set-value 'SearchBox' 'file_' -a $AppPid
        Invoke-Ui wait-for 'ResultRow-0' -a $AppPid --value 'file_' --contains -t 5000
    }
    # Target the main window by HWND for the scroll (winapp auto-picks the wrong
    # window when several are open).
    $mainHwnd = (Invoke-Ui list-windows -a $AppPid --json 2>$null | ConvertFrom-Json |
        Where-Object { $_.title -eq 'FindMyFiles' } | Select-Object -First 1).hwnd
    $beforeRows = Get-VisibleResultRows
    $beforeMax = ($beforeRows | ForEach-Object {
        [int]($_.automationId -replace '^ResultRow-', '')
    } | Measure-Object -Maximum).Maximum
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'C-prescroll.png') 2>$null
    # Scroll the list down repeatedly to force page fetches beyond the first
    # realized window.
    Test-UI 'Scroll: page down through several virtualization windows' {
        for ($i = 0; $i -lt 8; $i++) {
            Invoke-Ui scroll 'ResultsList' -w $mainHwnd --direction down 2>&1 | Out-Null
            Start-Sleep -Milliseconds 150
        }
        # scroll's exit code is what Test-UI checks; force success of the loop.
        $global:LASTEXITCODE = 0
    }
    Test-UI 'Scroll: newly realized rows are populated and advanced' {
        $afterRows = Get-VisibleResultRows
        if ($afterRows.Count -eq 0) { throw 'no visible result rows after scrolling' }
        $blank = @($afterRows | Where-Object {
            [string]::IsNullOrWhiteSpace($_.name) -or $_.name -notlike '*file_*'
        })
        if ($blank.Count -gt 0) {
            throw "$($blank.Count) visible row(s) were blank or stale after scrolling"
        }
        $afterMax = ($afterRows | ForEach-Object {
            [int]($_.automationId -replace '^ResultRow-', '')
        } | Measure-Object -Maximum).Maximum
        if ($afterMax -le $beforeMax) {
            throw "virtualized viewport did not advance (before=$beforeMax, after=$afterMax)"
        }
        $global:LASTEXITCODE = 0
    }

    # ── No-results empty state: a needle that matches nothing shows the overlay;
    #    a matching needle hides it again. The overlay title is a normal TextBlock
    #    (not a virtualized row), so winapp can see it.
    Test-UI 'NoResults: empty state shows for a no-match query' {
        Invoke-Ui set-value 'SearchBox' 'zzz_nomatch_zzz' -a $AppPid
        Invoke-Ui wait-for 'ResultRow-0' -w $mainHwnd --gone -t 5000
        if ($LASTEXITCODE -eq 0) {
            Invoke-Ui wait-for 'NoResultsTitle' -w $mainHwnd -t 3000
        }
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'C-noresults.png') 2>$null
    Test-UI 'NoResults: matching query restores rows and clears the overlay' {
        Invoke-Ui set-value 'SearchBox' 'file_0' -a $AppPid
        Invoke-Ui wait-for 'ResultRow-0' -w $mainHwnd --value 'file_0' --contains -t 5000
        if ($LASTEXITCODE -eq 0) {
            Invoke-Ui wait-for 'NoResultsTitle' -w $mainHwnd --gone -t 3000
        }
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'C-scroll.png') 2>$null
    Invoke-Ui set-value 'SearchBox' '' -a $AppPid 2>$null | Out-Null
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase D — diagnostics: DiagToggle opens the perf panel
#   DiagToggle is the MenuFlyoutItem in the MAIN window's OptionsButton (gear)
#   flyout. It calls App.ToggleDiagnostics, which now opens PerfPanel
#   (AutomationId=PerfPanel) in a SEPARATE top-level DiagnosticsWindow rather
#   than inside the main window. So the gear/menu invokes still target the main
#   window (-a $AppPid), but the PerfPanel assertion + screenshot must target the
#   new diagnostics window by its HWND. We discover that window with
#   `list-windows --json` and pick the one whose title is neither the main
#   window ('FindMyFiles') nor the transient menu/flyout host ('PopupHost').
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-DiagPhase {
    Write-Host "`n=== Phase D: diagnostics panel ===" -ForegroundColor Cyan

    # DiagToggle is now a button inside the settings dialog; clicking it closes the
    # dialog (Hide) and opens the diagnostics window (a separate top-level window).
    Test-UI 'Diag: open the perf panel via DiagToggle' {
        Open-Settings
        Invoke-Ui invoke 'DiagToggle' -a $AppPid
    }

    # The panel now lives in its own top-level window. Enumerate the process's
    # windows and pick the one that is neither the main window ('FindMyFiles') nor
    # a transient menu/flyout host ('PopupHost'); that is the DiagnosticsWindow.
    $script:diagHwnd = $null
    Test-UI 'Diag: diagnostics window opened as a separate top-level window' {
        # Give the new window a moment to materialise in the automation tree.
        Start-Sleep -Milliseconds 600
        $windows = @(Invoke-Ui list-windows -a $AppPid --json 2>$null | ConvertFrom-Json)
        $diag = $windows | Where-Object {
            $_.title -ne 'FindMyFiles' -and $_.title -ne 'PopupHost'
        } | Select-Object -First 1
        if ($null -eq $diag) {
            throw "no separate diagnostics window found (titles: $(($windows | ForEach-Object { $_.title }) -join ', '))"
        }
        $script:diagHwnd = $diag.hwnd
        $global:LASTEXITCODE = 0
    }

    # PerfPanel must be present INSIDE the diagnostics window, not the main one.
    Test-UI 'Diag: PerfPanel is shown in the diagnostics window' {
        if ($null -eq $script:diagHwnd) { throw 'no diagnostics window HWND captured' }
        Invoke-Ui wait-for 'PerfPanel' -w $script:diagHwnd -t 3000
    }

    # The memory card surfaces the process footprint (the headline new figure);
    # its value TextBlock carries the PerfProcessMem hook.
    Test-UI 'Diag: memory card shows the process footprint' {
        if ($null -eq $script:diagHwnd) { throw 'no diagnostics window HWND captured' }
        Invoke-Ui wait-for 'PerfProcessMem' -w $script:diagHwnd -t 3000
    }
    if ($script:diagHwnd) {
        Invoke-Ui screenshot -w $script:diagHwnd -o (Join-Path $OutDir 'D-perfpanel.png') 2>$null
    }
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase E — fault injection (DEBUG --fake-engine only; -IncludeFaults)
#   !!panic  → SearchAsync throws EngineException → surfaced into NotifyBar as an
#              error InfoBar; the app must NOT crash (window + tree survive).
#   !!lag    → every page fetch takes 250ms; results still publish with no blank
#              rows and the window stays responsive.
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-FaultPhase {
    Write-Host "`n=== Phase E: fault injection (DEBUG --fake-engine) ===" -ForegroundColor Cyan

    Test-UI 'Fault: !!panic surfaces into NotifyBar' {
        Invoke-Ui set-value 'SearchBox' '!!panic' -a $AppPid
        Start-Sleep -Milliseconds 600
        Invoke-Ui wait-for 'NotifyBar' -a $AppPid -t 3000
    }
    Test-UI 'Fault: app still alive after !!panic (SearchBox responds)' {
        Invoke-Ui set-value 'SearchBox' 'file_1' -a $AppPid
        Invoke-Ui wait-for 'SearchBox' -a $AppPid --value 'file_1' -t 2000
    }
    Test-UI 'Fault: !!lag still publishes results without crashing' {
        Invoke-Ui set-value 'SearchBox' '!!lag file_2' -a $AppPid
        Start-Sleep -Milliseconds 800
        Invoke-Ui wait-for 'ResultsList' -a $AppPid -t 3000
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'E-faults.png') 2>$null
    Invoke-Ui set-value 'SearchBox' '' -a $AppPid 2>$null | Out-Null
}

# ── Orchestration ─────────────────────────────────────────────────────────────
Write-Host 'FindMyFiles UI automation smoke suite' -ForegroundColor Cyan

$ownsApp = $false
try {
    if ($StableSmoke) {
        if ($PSCmdlet.ParameterSetName -ne 'Exe') {
            throw '-StableSmoke requires -ExePath.'
        }
        Write-Host "`n=== Shipping artifact smoke (no test seams) ===" -ForegroundColor Cyan
        $script:AppPid = Start-StableApp -Exe $ExePath
        $ownsApp = $true
        Test-UI 'Shipping artifact exposes actionable WinUI content' {
            # Layout-only panels are intentionally omitted from UIA's control
            # view. Accept the primary control from either real startup state.
            $setupProbe = Invoke-Ui wait-for 'EnableSearch' -a $script:AppPid -t 5000 2>&1
            if ($LASTEXITCODE -ne 0) {
                Invoke-Ui wait-for 'SearchBox' -a $script:AppPid -t 10000
            }
        }
        Test-UI 'Shipping artifact remains alive after initialization' {
            if (-not (Get-Process -Id $script:AppPid -ErrorAction SilentlyContinue)) {
                throw 'Shipping app exited during initialization.'
            }
        }
        Invoke-Ui screenshot -a $script:AppPid -o (Join-Path $OutDir 'stable-smoke.png') 2>$null
    } elseif ($PSCmdlet.ParameterSetName -eq 'Exe') {
        # Standalone mode: drive both the setup screen and the fake-engine phases off
        # one published exe path. Phase A spins its own --engine=unavailable process; the
        # rest share a --fake-engine process.
        Invoke-SetupPhase -Exe $ExePath
        $script:AppPid = Start-App -Exe $ExePath -AppArgs @('--fake-engine')
        $ownsApp = $true
    } else {
        # PID mode (the `just ui-test` recipe): the recipe already launched the exe
        # under --fake-engine and handed us its PID. The setup phase needs its own
        # --engine=unavailable process; if ExePath wasn't supplied we skip it and note why.
        if ($ExePath) {
            Invoke-SetupPhase -Exe $ExePath
        } else {
            Write-Host "`n=== Phase A skipped (no -ExePath; PID mode can't relaunch --engine=unavailable) ===" -ForegroundColor Yellow
            $script:results += @{ name = 'Setup phase'; status = 'SKIP'; detail = 'pass -ExePath to exercise --engine=unavailable' }
        }
        $ownsApp = $false
    }

    if (-not $StableSmoke) {
        Invoke-SearchPhase
        Invoke-ServiceSurfacePhase
        Invoke-ScrollPhase
        Invoke-DiagPhase
        if ($IncludeFaults) {
            Invoke-FaultPhase
        } else {
            Write-Host "`n=== Phase E skipped (pass -IncludeFaults; requires a DEBUG bundle) ===" -ForegroundColor Yellow
            $script:results += @{ name = 'Fault phase'; status = 'SKIP'; detail = 'requires DEBUG --fake-engine; pass -IncludeFaults' }
        }
    }
}
finally {
    # Always tear down the instance we launched — even if a phase threw — so the
    # run never leaves an orphaned process or a ghost Alt+Tab window behind.
    if ($ownsApp -and $script:AppPid) {
        Stop-AppGracefully $script:AppPid
    }
    # Preserve the test-owned app log before deleting its isolated state. UIA
    # failures often happen after the window has disappeared, and a screenshot
    # alone cannot explain startup/XAML/dispatcher failures. This contains no
    # user profile state: every process in the suite receives DataDir above.
    $logSource = $script:LogSource
    $logArtifact = Join-Path $script:OutRoot 'logs'
    if (Test-Path -LiteralPath $logArtifact) {
        $resolvedLogArtifact = [IO.Path]::GetFullPath($logArtifact)
        $resolvedRootPrefix = [IO.Path]::GetFullPath($script:OutRoot).TrimEnd(
            [IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        if ($resolvedLogArtifact.StartsWith(
            $resolvedRootPrefix,
            [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $resolvedLogArtifact -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path -LiteralPath $logSource) {
        Copy-Item -LiteralPath $logSource -Destination $logArtifact -Recurse -Force
    }
    # Delete only the exact, unique directory created under this run's artifact
    # root. The containment check prevents an accidental broad recursive delete.
    $resolvedData = [IO.Path]::GetFullPath($script:DataDir)
    $resolvedRoot = [IO.Path]::GetFullPath($script:OutRoot).TrimEnd(
        [IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if ($resolvedData.StartsWith($resolvedRoot, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $resolvedData).StartsWith('state-', [StringComparison]::Ordinal)) {
        Remove-Item -LiteralPath $resolvedData -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# ── Results ───────────────────────────────────────────────────────────────────
$resultPath = Join-Path $OutDir 'test-results.json'
$script:results | ConvertTo-Json -Depth 4 | Out-File $resultPath
Write-Host "`nPassed: $script:pass | Failed: $script:fail" -ForegroundColor Cyan
Write-Host "Results: $resultPath"
$script:results | Where-Object { $_.status -eq 'FAIL' } | ForEach-Object {
    Write-Host "  FAIL: $($_.name) — $($_.detail)" -ForegroundColor Red
}
if ($script:fail -gt 0) { exit 1 } else { exit 0 }
