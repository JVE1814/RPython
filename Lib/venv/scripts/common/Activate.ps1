<#
.Synopsis
Activate the virtual environment in the current PowerShell session.

.Description
Puts the environment's script directory first on PATH, sets
$env:VIRTUAL_ENV and prefixes the prompt.  Run "deactivate" to undo.

.Parameter Prompt
Prompt prefix to show instead of the one configured for the environment.

.Example
. <venv>/bin/Activate.ps1
#>
param(
    [Parameter(Mandatory = $false)]
    [String]
    $Prompt
)

function global:deactivate([Switch] $NonDestructive) {
    if (Test-Path -Path Variable:global:_VENV_SAVED_PATH) {
        $env:PATH = $global:_VENV_SAVED_PATH
        Remove-Variable -Name _VENV_SAVED_PATH -Scope Global
    }
    if (Test-Path -Path Variable:global:_VENV_SAVED_PYTHONHOME) {
        $env:PYTHONHOME = $global:_VENV_SAVED_PYTHONHOME
        Remove-Variable -Name _VENV_SAVED_PYTHONHOME -Scope Global
    }
    if (Test-Path -Path Function:global:_VENV_SAVED_PROMPT) {
        Copy-Item -Path Function:global:_VENV_SAVED_PROMPT -Destination Function:global:prompt
        Remove-Item -Path Function:global:_VENV_SAVED_PROMPT
    }
    if (Test-Path -Path Env:VIRTUAL_ENV) {
        Remove-Item -Path Env:VIRTUAL_ENV
    }
    if (Test-Path -Path Env:VIRTUAL_ENV_PROMPT) {
        Remove-Item -Path Env:VIRTUAL_ENV_PROMPT
    }
    if (-not $NonDestructive) {
        Remove-Item -Path Function:global:deactivate
    }
}

# Leave any environment that is currently active.
deactivate -NonDestructive

$VenvDir = __VENV_DIR__
$BinName = __VENV_BIN_NAME__
if (-not $Prompt) {
    $Prompt = __VENV_PROMPT__
}

$env:VIRTUAL_ENV = $VenvDir
$env:VIRTUAL_ENV_PROMPT = $Prompt

$global:_VENV_SAVED_PATH = $env:PATH
$env:PATH = (Join-Path -Path $VenvDir -ChildPath $BinName) + [System.IO.Path]::PathSeparator + $env:PATH

if (Test-Path -Path Env:PYTHONHOME) {
    $global:_VENV_SAVED_PYTHONHOME = $env:PYTHONHOME
    Remove-Item -Path Env:PYTHONHOME
}

if (-not $env:VIRTUAL_ENV_DISABLE_PROMPT) {
    Copy-Item -Path Function:global:prompt -Destination Function:global:_VENV_SAVED_PROMPT
    function global:prompt {
        Write-Host -NoNewline -ForegroundColor Green "($env:VIRTUAL_ENV_PROMPT) "
        _VENV_SAVED_PROMPT
    }
}
