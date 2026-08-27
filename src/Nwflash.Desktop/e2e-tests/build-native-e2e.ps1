#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$desktopRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$targetRoot = Join-Path $desktopRoot 'src-tauri\target\e2e-native'
$priorTarget = $env:CARGO_TARGET_DIR

function Invoke-Checked {
    param([Parameter(Mandatory)][scriptblock]$Command, [Parameter(Mandatory)][string]$Description)

    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Description failed with exit code $LASTEXITCODE."
    }
}

try {
    $env:CARGO_TARGET_DIR = $targetRoot
    Invoke-Checked -Description 'E2E frontend build' -Command {
        npm --prefix $desktopRoot run build:e2e
    }
    Invoke-Checked -Description 'Native E2E Tauri build' -Command {
        npm --prefix $desktopRoot run tauri -- build --debug --features e2e --config (Join-Path $desktopRoot 'src-tauri\tauri.e2e.conf.json') --no-sign --no-bundle
    }
    $binary = Join-Path $targetRoot 'debug\nwflash-desktop.exe'
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf) -or (Get-Item -LiteralPath $binary).Length -le 0) {
        throw "Native E2E binary is missing: $binary"
    }
}
finally {
    if ($null -eq $priorTarget) {
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    }
    else {
        $env:CARGO_TARGET_DIR = $priorTarget
    }
}
