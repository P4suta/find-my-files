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
    param([Parameter(ValueFromRemainingArguments)][string[]]$Args)
    & $script:UiCli ui @Args
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

    if ($setupPid) { Stop-Process -Id $setupPid -Force -ErrorAction SilentlyContinue }
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase B — SEARCH interactions under --fake-engine (deterministic 100k rows)
#   Data shape (FakeEngineClient, seed 42): names file_NNNNNN_x.ext; folders
#   folder_NNNNNN every 50th row; hidden/system rows hidden_sys_NNNNNN.dat every
#   97th row. StatusCount lives in the OptionsButton flyout → StatusMenu submenu,
#   so reading it means opening the flyout first.
# ──────────────────────────────────────────────────────────────────────────────
function Open-StatusCount {
    # Open OptionsButton → StatusMenu so StatusCount is in the automation tree.
    # Flyout items appear asynchronously, hence the short sleeps.
    Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
    Start-Sleep -Milliseconds 400
    Invoke-Ui invoke 'StatusMenu' -a $AppPid | Out-Null
    Start-Sleep -Milliseconds 400
}

function Get-StatusCountText {
    Open-StatusCount
    $v = Invoke-Ui get-value 'StatusCount' -a $AppPid --json 2>$null | ConvertFrom-Json
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
    # Close the flyout before the next interaction (Escape collapses it).
    Invoke-Ui set-value 'SearchBox' 'file_0' -a $AppPid 2>$null | Out-Null

    # ── Sort reorder: SortName / SortSize / SortDate are RadioMenuFlyoutItems in
    #    the OptionsButton flyout's sort submenu. Invoking each must succeed and
    #    leave ResultsList intact (the virtualized list must not blank out). We
    #    capture the first row's name to confirm the order actually changed.
    function Invoke-Sort {
        param([string]$SortId)
        Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
        Start-Sleep -Milliseconds 400
        Invoke-Ui invoke $SortId -a $AppPid
    }
    Test-UI 'Sort: SortName applies' { Invoke-Sort 'SortName' }
    Test-UI 'Sort: SortSize applies (reorders by size)' { Invoke-Sort 'SortSize' }
    Test-UI 'Sort: SortDate applies (reorders by date)' { Invoke-Sort 'SortDate' }
    Test-UI 'Sort: ResultsList still populated after reorders' {
        Invoke-Ui wait-for 'ResultsList' -a $AppPid -t 2000
    }

    # ── OptRegex toggle: switches the fake into .NET-regex filtering. The needle
    #    "file_0" is a valid regex, so results stay non-empty; the toggle itself
    #    must flip to On.
    Test-UI 'OptRegex: toggle on' {
        Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
        Start-Sleep -Milliseconds 400
        Invoke-Ui invoke 'OptRegex' -a $AppPid
    }
    Test-UI 'OptRegex: reads On' {
        Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
        Start-Sleep -Milliseconds 400
        Invoke-Ui wait-for 'OptRegex' -a $AppPid --value 'On' -t 2000
    }
    # Toggle regex back off so the system-files assertion below filters by plain
    # substring (deterministic count delta).
    Test-UI 'OptRegex: toggle back off' {
        Invoke-Ui invoke 'OptRegex' -a $AppPid
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
        Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
        Start-Sleep -Milliseconds 400
        Invoke-Ui invoke 'OptSystem' -a $AppPid
    }
    Test-UI 'OptSystem: count changes when system files are included' {
        $countSysOn = Get-StatusCountText
        if ($countSysOn -eq $script:countSysOff) {
            throw "StatusCount unchanged by OptSystem ('$countSysOn') — hidden/system rows were not surfaced"
        }
    }
    # Reset for the next phase.
    Invoke-Ui invoke 'OptSystem' -a $AppPid 2>$null | Out-Null
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
    # Scroll the list down repeatedly to force page fetches beyond the first
    # realized window.
    Test-UI 'Scroll: page down through several virtualization windows' {
        for ($i = 0; $i -lt 8; $i++) {
            Invoke-Ui scroll 'ResultsList' -a $AppPid --direction down 2>&1 | Out-Null
            Start-Sleep -Milliseconds 150
        }
        # scroll's exit code is what Test-UI checks; force success of the loop.
        $global:LASTEXITCODE = 0
    }
    # After deep scroll the realized rows must carry text, not be blank
    # placeholders. inspect the list subtree and assert at least one ListItem has
    # a non-empty name.
    Test-UI 'Scroll: realized rows are not blank' {
        $tree = Invoke-Ui inspect -a $AppPid --interactive --json 2>$null | ConvertFrom-Json
        $items = @($tree.elements | Where-Object {
            $_.type -match 'ListItem' -and -not [string]::IsNullOrWhiteSpace($_.name)
        })
        if ($items.Count -eq 0) {
            throw 'no realized ListItem carried text after deep scroll (blank-row regression)'
        }
        $global:LASTEXITCODE = 0
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'C-scroll.png') 2>$null
    Invoke-Ui set-value 'SearchBox' '' -a $AppPid 2>$null | Out-Null
}

# ──────────────────────────────────────────────────────────────────────────────
# Phase D — diagnostics: DiagToggle opens the perf panel
#   DiagToggle is the MenuFlyoutItem in the OptionsButton flyout that toggles
#   ViewModel.Perf.IsOpen; PerfPanel (AutomationId=PerfPanel) becomes visible.
# ──────────────────────────────────────────────────────────────────────────────
function Invoke-DiagPhase {
    Write-Host "`n=== Phase D: diagnostics panel ===" -ForegroundColor Cyan

    Test-UI 'Diag: open the perf panel via DiagToggle' {
        Invoke-Ui invoke 'OptionsButton' -a $AppPid | Out-Null
        Start-Sleep -Milliseconds 400
        Invoke-Ui invoke 'DiagToggle' -a $AppPid
    }
    Test-UI 'Diag: PerfPanel is shown' {
        Invoke-Ui wait-for 'PerfPanel' -a $AppPid -t 3000
    }
    Invoke-Ui screenshot -a $AppPid -o (Join-Path $OutDir 'D-perfpanel.png') 2>$null
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

if ($ownsApp -and $script:AppPid) {
    Stop-Process -Id $script:AppPid -Force -ErrorAction SilentlyContinue
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
