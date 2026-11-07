param(
    [Parameter(Mandatory)][string]$TestScriptPath,
    [Parameter(Mandatory)][ValidateSet('delayed-close', 'close-invoke-error', 'close-wait-error', 'close-ready-error', 'open-invoke-error', 'open-wait-error', 'wait-for-cancel')][string]$Scenario
)

$ErrorActionPreference = 'Stop'
if ($Scenario -eq 'wait-for-cancel') {
    [Threading.Thread]::Sleep([Threading.Timeout]::Infinite)
    throw 'The cancellation fixture resumed without termination.'
}
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $TestScriptPath, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'UI script has parse errors.' }
foreach ($functionName in @('Open-Settings', 'Close-Settings')) {
    $definitions = @($ast.FindAll({
        param([System.Management.Automation.Language.Ast]$node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -ceq $functionName
    }, $true))
    if ($definitions.Count -ne 1) { throw "Expected one $functionName definition." }
    . ([scriptblock]::Create($definitions[0].Extent.Text))
}

$AppPid = 41
$script:clock = 0
$script:dialog = 'Open'
$script:closeAt = 700
$script:readyAt = 900
$script:opened = 0

function Start-Sleep {
    param([int]$Milliseconds)
    $script:clock += $Milliseconds
}

function Invoke-Ui {
    $verb = [string]$args[0]
    $selector = [string]$args[1]
    $global:LASTEXITCODE = 0
    switch ("${verb}:${selector}") {
        'invoke:CloseButton' {
            if ($Scenario -eq 'close-invoke-error') { $global:LASTEXITCODE = 17; return }
            $script:dialog = 'Closing'
        }
        'invoke:OptionsButton' {
            if ($Scenario -eq 'open-invoke-error') { $global:LASTEXITCODE = 11; return }
            if ($script:dialog -eq 'Closing' -and $script:clock -lt $script:readyAt) { return }
            $script:dialog = 'Open'
            $script:opened++
        }
        'wait-for:SettingsDialog' {
            if ($args -contains '--gone') {
                if ($Scenario -eq 'close-wait-error') { $global:LASTEXITCODE = 19; return }
                $script:clock = [Math]::Max($script:clock, $script:closeAt)
                $script:dialog = 'Closed'
            } else {
                if ($Scenario -eq 'open-wait-error') { $global:LASTEXITCODE = 13; return }
                if ($script:dialog -ne 'Open') { $global:LASTEXITCODE = 1 }
            }
        }
        'wait-for:OptionsButton' {
            if ($args -notcontains 'IsEnabled' -or $args -notcontains 'True') {
                throw 'Readiness must require an enabled options button.'
            }
            if ($Scenario -eq 'close-ready-error') { $global:LASTEXITCODE = 23; return }
            $script:clock = [Math]::Max($script:clock, $script:readyAt)
        }
        default { throw "Unexpected UI operation ${verb}:${selector}." }
    }
}

if ($Scenario -eq 'delayed-close') {
    Close-Settings
    if ($script:dialog -ne 'Closed' -or $script:clock -lt $script:readyAt) {
        throw 'Close returned before dismissal and interactivity completed.'
    }
    Open-Settings
    if ($script:dialog -ne 'Open' -or $script:opened -ne 1) {
        throw 'The next open request was lost.'
    }
} else {
    $expectedCode = switch ($Scenario) {
        'close-invoke-error' { 17 }
        'close-wait-error' { 19 }
        'close-ready-error' { 23 }
        'open-invoke-error' { 11 }
        'open-wait-error' { 13 }
    }
    $caught = $false
    try {
        if ($Scenario.StartsWith('open-', [StringComparison]::Ordinal)) {
            Open-Settings
        } else {
            Close-Settings
        }
    } catch {
        if ($_.Exception.Message -notmatch "exit $expectedCode\b") { throw }
        $caught = $true
    }
    if (-not $caught) { throw "UI failure $expectedCode was hidden." }
}

Write-Output "PASS $Scenario"
