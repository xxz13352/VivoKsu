#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$desktopRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$targetRoot = Join-Path $desktopRoot 'src-tauri\target\e2e-native'
$e2eVerificationKeyB64 = '11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo='
$priorTarget = $env:CARGO_TARGET_DIR
$priorVerificationKey = $env:NWFLASH_SESSION_VERIFY_KEY_B64

if ([Convert]::FromBase64String($e2eVerificationKeyB64).Length -ne 32) {
    throw 'The deterministic E2E verification key must decode to exactly 32 bytes.'
}

function Invoke-Checked {
    param([Parameter(Mandatory)][scriptblock]$Command, [Parameter(Mandatory)][string]$Description)

    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Description failed with exit code $LASTEXITCODE."
    }
}

try {
    $env:CARGO_TARGET_DIR = $targetRoot
    # RFC 8032 test-vector public key. It is public test data and is never used by production builds.
    $env:NWFLASH_SESSION_VERIFY_KEY_B64 = $e2eVerificationKeyB64
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
    if ($null -eq $priorVerificationKey) {
        Remove-Item Env:NWFLASH_SESSION_VERIFY_KEY_B64 -ErrorAction SilentlyContinue
    }
    else {
        $env:NWFLASH_SESSION_VERIFY_KEY_B64 = $priorVerificationKey
    }
}
