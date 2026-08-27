#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$entrypoints = @(
    'scripts\New-TauriReleaseManifest.ps1'
    'scripts\Protect-NwflashRelease.ps1'
    'scripts\Publish-TauriRelease.ps1'
    'scripts\Sign-NwflashRelease.ps1'
    'scripts\Test-ProtectedBuildProfile.ps1'
    'scripts\Test-ProtectedRelease.ps1'
    'scripts\Test-PowerShellRuntimeBoundary.ps1'
    'scripts\Test-TauriCapabilityBoundary.ps1'
    'scripts\Test-TauriInstaller.ps1'
    'scripts\Test-TauriRelease.ps1'
    'scripts\Verify-ProtectedRelease.ps1'
    'scripts\Verify-TauriRelease.ps1'
    'scripts\vmp\accept-manual-output.ps1'
    'scripts\vmp\cleanup-generated-preflight.ps1'
    'scripts\vmp\prepare-manual-handoff.ps1'
    'scripts\vmp\protected-release-contract.ps1'
    'scripts\vmp\test-contracts.ps1'
    'scripts\vmp\verify-link-layout.ps1'
    'scripts\vmp\verify-sdk.ps1'
    'src\Nwflash.Desktop\e2e-tests\build-native-e2e.ps1'
)

foreach ($relative in $entrypoints) {
    $path = Join-Path $repo $relative
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "PowerShell entry point is missing: $relative"
    }

    $head = @(Get-Content -LiteralPath $path -TotalCount 3)
    if ($head.Count -lt 2 -or $head[0] -ne '#requires -Version 7.4' -or
        $head[1] -ne '#requires -PSEdition Core') {
        throw "PowerShell 7.4/Core preflight is missing or not first in: $relative"
    }

    $tokens = $null
    $errors = $null
    [void][Management.Automation.Language.Parser]::ParseFile(
        $path,
        [ref]$tokens,
        [ref]$errors
    )
    if ($errors.Count -ne 0) {
        $summary = ($errors | ForEach-Object {
            "line $($_.Extent.StartLineNumber): $($_.Message)"
        }) -join '; '
        throw "PowerShell parser rejected ${relative}: $summary"
    }
}

$e2ePackagePath = Join-Path $repo 'src\Nwflash.Desktop\e2e-tests\package.json'
$e2ePackage = Get-Content -Raw -LiteralPath $e2ePackagePath | ConvertFrom-Json
$pretest = [string]$e2ePackage.scripts.pretest
if ($pretest -notmatch '^pwsh\s' -or $pretest -match '(?i)^powershell(?:\.exe)?\s') {
    throw 'Native E2E pretest must invoke pwsh explicitly.'
}

foreach ($relative in @('scripts\vmp\README.md', 'packaging\vmprotect\README.md')) {
    $text = Get-Content -Raw -LiteralPath (Join-Path $repo $relative)
    if ($text -notmatch 'PowerShell 7\.4' -or $text -notmatch '\bpwsh\b') {
        throw "PowerShell 7.4/pwsh operator guidance is missing from: $relative"
    }
}

Write-Host 'PowerShell 7.4/Core runtime boundary passed.'
