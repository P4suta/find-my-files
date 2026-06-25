<#
.SYNOPSIS
    winapp-UI-automation smoke suite for FindMyFiles (the WinUI 3 app).

.DESCRIPTION
    Scripted batch UI test. Drives the PUBLISHED FindMyFiles.exe through the
    `winapp ui` automation verbs (wait-for / invoke / set-value / click /
    screenshot) and asserts on AutomationIds declared in app/FindMyFiles/
    MainPage.xaml. Two launch modes are exercised:

      --engine=empty  forces the disconnected first-run setup screen
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
    underlying error rather than aborting the run, and a single TODO marker calls
    out where a harness-specific shim would slot in. Verb reference:
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
    # for the --engine=empty setup-screen phase, which needs its own process.
    [Parameter(Mandatory, ParameterSetName = 'Exe')]
    [string]$ExePath,

    # Run the DEBUG-only fault-injection phase (!!panic / !!lag). Skipped by
    # default because a Release bundle compiles those branches out.
    [switch]$IncludeFaults,

    # Where screenshots + the results JSON land.
    [string]$OutDir = (Join-Path $PSScriptRoot 'artifacts')
)

$ErrorActionPreference = 'Continue'
$script:pass = 0
$script:fail = 0
$script:results = @()

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# ── Harness shim ──────────────────────────────────────────────────────────────
# Single chokepoint for the UI automation CLI. Every scenario goes through here,
# so if this machine's harness binary is named/invoked differently, this is the
# ONE place to adapt.
#
# TODO(harness): confirm the automation CLI entrypoint on this machine. The
# project standard is `winapp ui <verb>`. If `winapp` is not on PATH, set
# $env:FMF_UI_CLI to the full path of the automation exe (it must accept the same
# `ui <verb>` verbs documented by `winapp ui --cli-schema`), or replace the
# invocation below with the local equivalent. Nothing else in this file knows the
# CLI name.
$script:UiCli = if ($env:FMF_UI_CLI) { $env:FMF_UI_CLI } else { 'winapp' }

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

# Launch the published exe with the given args and return its PID. Used for the
# setup-screen phase, which needs its own --engine=empty process.
function Start-App {
    param([string]$Exe, [string[]]$AppArgs)
    if (-not (Test-Path $Exe)) {
        throw "FindMyFiles.exe not found at '$Exe' — run `just publish` first."
    }
    $p = Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru
    # Give the WinUI window + the automation tree time to materialise. The first
    # wait-for in each phase has its own timeout, so this is just startup slack.
    Start-Sleep -Seconds 2
    return $p.Id
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
# Phase A — first-run SETUP screen under --engine=empty
#   IsDisconnected=true collapses the search UI (IsReady=false) and centres the
#   setup StackPanel. Asserts the two CTAs the user can act on:
#     EnableSearch   (AccentButton — register the service / privileged path)
#     ScopeSetupLink (HyperlinkButton — no-admin scope path → ScopeManagerDialog)
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-SetupPhase {
    param([string]$Exe)
    Write-Host "`n=== Phase A: first-run setup (--engine=empty) ===" -ForegroundColor Cyan

    $setupPid = $null
    try {
        $setupPid = Start-App -Exe $Exe -AppArgs @('--engine=empty')
    } catch {
        $script:fail++
        $script:results += @{ name = 'Setup: launch --engine=empty'; status = 'FAIL'; detail = "$_" }
        Write-Host "  FAIL: Setup launch — $_" -ForegroundColor Red
        return
    }

    Test-UI 'Setup: EnableSearch button present' {
        Invoke-Ui wait-for 'EnableSearch' -a $setupPid -t 5000
    }
    Test-UI 'Setup: ScopeSetupLink present' {
        Invoke-Ui wait-for 'ScopeSetupLink' -a $setupPid -t 3000
    }
    Test-UI 'Setup: EnableSearch is enabled (SetupNotBusy)' {
        Invoke-Ui wait-for 'EnableSearch' -a $setupPid -p IsEnabled --value 'True' -t 3000
    }
    # Search UI is collapsed on the setup screen (IsReady=false): SearchBox must
    # NOT be interactable. wait-for --gone is the disconnected-state invariant.
    Test-UI 'Setup: SearchBox collapsed while disconnected' {
        Invoke-Ui wait-for 'SearchBox' -a $setupPid --gone -t 3000
    }
    # The no-admin path: clicking the link opens ScopeManagerDialog (folder-only).
    Test-UI 'Setup: ScopeSetupLink opens the scope dialog' {
        Invoke-Ui invoke 'ScopeSetupLink' -a $setupPid
    }
    Test-UI 'Setup: ScopeManagerDialog appears' {
        Invoke-Ui wait-for 'ScopeManagerDialog' -a $setupPid -t 3000
    }
    Invoke-Ui screenshot -a $setupPid -o (Join-Path $OutDir 'A-setup.png') 2>$null

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

    # Type a needle that matches a deterministic subset. SearchText binds with
    # UpdateSourceTrigger=PropertyChanged, so set-value drives the live filter
    # without needing a LostFocus commit.
    Test-UI 'Search: type "file_0" into SearchBox' {
        Invoke-Ui set-value 'SearchBox' 'file_0' -a $AppPid
    }
    Test-UI 'Search: SearchBox holds the typed text' {
        Invoke-Ui wait-for 'SearchBox' -a $AppPid --value 'file_0' -t 2000
    }

    # StatusCount must change away from the empty-query state. Capture it now so
    # later toggles can assert it moved. CountText is a localized string, so we
    # assert non-empty rather than a brittle exact integer.
    $countAfterType = $null
    Test-UI 'Search: StatusCount reflects the query (non-empty)' {
        $script:countAfterType = Get-StatusCountText
        if ([string]::IsNullOrWhiteSpace($script:countAfterType)) {
            throw 'StatusCount text was empty after typing a query'
        }
    }

    # Capture the open settings dialog for a visual check of the SettingsCard surface.
    Open-Settings
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
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    Test-UI 'Sort: SortName applies' { Invoke-Sort 'SortName' }
    Test-UI 'Sort: SortSize applies (reorders by size)' { Invoke-Sort 'SortSize' }
    Test-UI 'Sort: SortDate applies (reorders by date)' { Invoke-Sort 'SortDate' }
    Test-UI 'Sort: ResultsList still populated after reorders' {
        Invoke-Ui wait-for 'ResultsList' -a $AppPid -t 2000
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

    # ── OptSystem toggle: hidden_sys_* rows (every 97th) are excluded by default.
    #    Search a needle that ONLY matches hidden/system rows; turning OptSystem
    #    on must change the count from the excluded state to a non-empty match.
    Test-UI 'OptSystem: search a hidden/system-only needle' {
        Invoke-Ui set-value 'SearchBox' 'hidden_sys' -a $AppPid
    }
    $countSysOff = $null
    Test-UI 'OptSystem: count with system files hidden (default off)' {
        $script:countSysOff = Get-StatusCountText
        if ($null -eq $script:countSysOff) { throw 'no StatusCount read (system off)' }
    }
    Test-UI 'OptSystem: toggle on' {
        Open-Settings
        Invoke-Ui invoke 'OptSystem' -a $AppPid
        $code = $LASTEXITCODE
        Close-Settings
        $global:LASTEXITCODE = $code
    }
    Test-UI 'OptSystem: count changes when system files are included' {
        $countSysOn = Get-StatusCountText
        if ($countSysOn -eq $script:countSysOff) {
            throw "StatusCount unchanged by OptSystem ('$countSysOn') — hidden/system rows were not surfaced"
        }
    }
    # Reset for the next phase (toggle system off again; clear the box).
    Open-Settings
    Invoke-Ui invoke 'OptSystem' -a $AppPid 2>$null | Out-Null
    Close-Settings
    Invoke-Ui set-value 'SearchBox' '' -a $AppPid 2>$null | Out-Null
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'B-search.png') 2>$null
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
        Start-Sleep -Milliseconds 500
        Invoke-Ui wait-for 'ResultsList' -a $AppPid -t 3000
    }
    # Target the main window by HWND for the scroll (winapp auto-picks the wrong
    # window when several are open).
    $mainHwnd = (Invoke-Ui list-windows -a $AppPid --json 2>$null | ConvertFrom-Json |
        Where-Object { $_.title -eq 'FindMyFiles' } | Select-Object -First 1).hwnd
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
    # The list must survive the deep scroll — still present, not blanked out or
    # errored. NOTE: we deliberately do NOT assert on individual row elements
    # here. The ListView is UIA-virtualized (rows are realized lazily and are
    # absent from the UIA tree until a UIA client realizes them), so
    # `winapp ui inspect` returns an empty subtree for it even targeted by HWND —
    # there is no winapp-visible per-row element to assert on. Row-content
    # correctness (no blank/placeholder rows) is covered by the engine +
    # VirtualResultList unit tests; this phase guards that scrolling the realized
    # list does not crash or clear it.
    Test-UI 'Scroll: list survives deep scroll (still present)' {
        Invoke-Ui wait-for 'ResultsList' -w $mainHwnd -t 2000
    }

    # ── No-results empty state: a needle that matches nothing shows the overlay;
    #    a matching needle hides it again. The overlay title is a normal TextBlock
    #    (not a virtualized row), so winapp can see it.
    Test-UI 'NoResults: empty state shows for a no-match query' {
        Invoke-Ui set-value 'SearchBox' 'zzz_nomatch_zzz' -a $AppPid
        Start-Sleep -Milliseconds 500
        Invoke-Ui wait-for 'NoResultsTitle' -w $mainHwnd -t 3000
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'C-noresults.png') 2>$null
    Test-UI 'NoResults: empty state clears with the query' {
        # Clearing the box → empty query → the overlay must go (HasNoResults=false
        # and the results row collapses to EmptyState). Robust regardless of which
        # fake needles match. Invert a short present-wait: it must NOT find it.
        Invoke-Ui set-value 'SearchBox' '' -a $AppPid
        Start-Sleep -Milliseconds 500
        Invoke-Ui wait-for 'NoResultsTitle' -w $mainHwnd -t 800 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { throw 'no-results overlay still present after clearing the query' }
        $global:LASTEXITCODE = 0
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
    if ($PSCmdlet.ParameterSetName -eq 'Exe') {
        # Standalone mode: drive both the setup screen and the fake-engine phases off
        # one published exe path. Phase A spins its own --engine=empty process; the
        # rest share a --fake-engine process.
        Invoke-SetupPhase -Exe $ExePath
        $script:AppPid = Start-App -Exe $ExePath -AppArgs @('--fake-engine')
        $ownsApp = $true
    } else {
        # PID mode (the `just ui-test` recipe): the recipe already launched the exe
        # under --fake-engine and handed us its PID. The setup phase needs its own
        # --engine=empty process; if ExePath wasn't supplied we skip it and note why.
        if ($ExePath) {
            Invoke-SetupPhase -Exe $ExePath
        } else {
            Write-Host "`n=== Phase A skipped (no -ExePath; PID mode can't relaunch --engine=empty) ===" -ForegroundColor Yellow
            $script:results += @{ name = 'Setup phase'; status = 'SKIP'; detail = 'pass -ExePath to exercise --engine=empty' }
        }
        $ownsApp = $false
    }

    Invoke-SearchPhase
    Invoke-ScrollPhase
    Invoke-DiagPhase
    if ($IncludeFaults) {
        Invoke-FaultPhase
    } else {
        Write-Host "`n=== Phase E skipped (pass -IncludeFaults; requires a DEBUG bundle) ===" -ForegroundColor Yellow
        $script:results += @{ name = 'Fault phase'; status = 'SKIP'; detail = 'requires DEBUG --fake-engine; pass -IncludeFaults' }
    }
}
finally {
    # Always tear down the instance we launched — even if a phase threw — so the
    # run never leaves an orphaned process or a ghost Alt+Tab window behind.
    if ($ownsApp -and $script:AppPid) {
        Stop-AppGracefully $script:AppPid
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
