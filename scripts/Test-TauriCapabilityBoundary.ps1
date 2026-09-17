#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$desktop = Join-Path $repo 'src\Nwflash.Desktop'
$tauri = Join-Path $desktop 'src-tauri'
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')

function Invoke-Checked {
    param([Parameter(Mandatory)][scriptblock]$Command, [Parameter(Mandatory)][string]$Description)
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Description failed with exit code $LASTEXITCODE." }
}

function Read-GeneratedCapabilities {
    param([Parameter(Mandatory)][string]$TargetRoot, [Parameter(Mandatory)][ValidateSet('debug', 'release')][string]$Profile)
    $files = @(Get-ChildItem -LiteralPath (Join-Path $TargetRoot "$Profile\build") -Recurse -File -Filter 'capabilities.json' |
        Where-Object { $_.FullName -match '(?i)\\nwflash-desktop-[^\\]+\\out\\capabilities\.json$' })
    if ($files.Count -ne 1) { throw "Expected exactly one generated desktop capabilities.json under $TargetRoot; found $($files.Count)." }
    Get-Content -Raw -LiteralPath $files[0].FullName | ConvertFrom-Json -AsHashtable
}

function Assert-WdioGraph {
    param([switch]$E2E)
    $arguments = @('tree', '--manifest-path', (Join-Path $tauri 'Cargo.toml'), '-p', 'nwflash-desktop', '--edges', 'normal,build', '--no-default-features')
    if ($E2E) { $arguments += @('--features', 'e2e') }
    $tree = (& cargo @arguments 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Cargo dependency graph probe failed.' }
    $containsWdio = $tree -match '(?m)^.*tauri-plugin-wdio(?:-webdriver)? v'
    if ($E2E -and (-not ($tree -match 'tauri-plugin-wdio v') -or -not ($tree -match 'tauri-plugin-wdio-webdriver v'))) {
        throw 'E2E Cargo graph does not activate both WDIO plugins.'
    }
    if (-not $E2E -and $containsWdio) { throw 'Production Cargo graph activates a WDIO plugin.' }
}

$productionTarget = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-capability-prod-' + [Guid]::NewGuid().ToString('N'))
$e2eTarget = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-capability-e2e-' + [Guid]::NewGuid().ToString('N'))
$priorTarget = $env:CARGO_TARGET_DIR
$priorBuildId = $env:NWFLASH_BUILD_ID
try {
    # Release probes distinguish an unavailable VMProtect runtime (43) from a
    # missing compiled build identity (46). Supply a deterministic test build
    # ID so this boundary exercises the intended unavailable-runtime branch.
    $env:NWFLASH_BUILD_ID = 'capability-boundary:1'
    Assert-WdioGraph
    Assert-WdioGraph -E2E
    Invoke-Checked -Description 'Frontend production build' -Command { npm --prefix $desktop run build }
    $env:CARGO_TARGET_DIR = $productionTarget
    Invoke-Checked -Description 'Native production Tauri build' -Command {
        npm --prefix $desktop run tauri -- build --no-sign --no-bundle
    }
    $production = Read-GeneratedCapabilities -TargetRoot $productionTarget -Profile release
    if (@($production.Keys) -ne 'default') { throw 'Generated production capabilities must contain only default.' }
    $productionPermissions = @($production.default.permissions)
    if ($productionPermissions | Where-Object { $_ -match '^(wdio|wdio-webdriver):' }) { throw 'Generated production capability grants WDIO.' }
    if (Get-ChildItem -LiteralPath (Join-Path $productionTarget 'release\build') -Directory -Filter 'tauri-plugin-wdio*') { throw 'Production native build generated a WDIO plugin.' }
    $productionBinary = Resolve-FullyQualifiedLeaf (Join-Path $productionTarget 'release\nwflash-desktop.exe')
    $productionEffective = (& $productionBinary --nwflash-effective-capabilities-probe 2>&1 | Out-String).Trim() | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or @($productionEffective.capabilities).Count -ne 1 -or
        [string]$productionEffective.capabilities[0] -ne 'default') {
        throw 'Built production context does not select exactly the default capability.'
    }
    $probeLine = (& $productionBinary --nwflash-protected-release-probe 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 43) { throw "Unprotected production probe must fail with exit code 43; found $LASTEXITCODE." }
    try { $probe = $probeLine | ConvertFrom-Json } catch { throw 'Unprotected production probe did not emit machine-readable JSON.' }
    if ([bool]$probe.probe_available -or $null -ne $probe.VMProtectIsProtected -or $null -ne $probe.VMProtectIsValidImageCRC) {
        throw 'Unprotected production probe claimed VMProtect runtime signals.'
    }

    Invoke-Checked -Description 'Frontend native E2E build' -Command { npm --prefix $desktop run build:e2e }
    $env:CARGO_TARGET_DIR = $e2eTarget
    Invoke-Checked -Description 'Native E2E Tauri build' -Command {
        npm --prefix $desktop run tauri -- build --debug --features e2e --config (Join-Path $tauri 'tauri.e2e.conf.json') --no-sign --no-bundle
    }
    $e2e = Read-GeneratedCapabilities -TargetRoot $e2eTarget -Profile debug
    if (@($e2e.Keys) -ne 'default') { throw 'E2E build-script metadata must remain limited to the production default capability.' }
    $pluginBuilds = @(Get-ChildItem -LiteralPath (Join-Path $e2eTarget 'debug\build') -Directory -Filter 'tauri-plugin-wdio*' | Select-Object -ExpandProperty Name)
    foreach ($plugin in @('tauri-plugin-wdio-', 'tauri-plugin-wdio-webdriver-')) {
        if (-not ($pluginBuilds | Where-Object { $_.StartsWith($plugin, [StringComparison]::Ordinal) })) {
            throw "E2E native build did not generate $plugin."
        }
    }
    $e2eBinary = Resolve-FullyQualifiedLeaf (Join-Path $e2eTarget 'debug\nwflash-desktop.exe')
    $e2eEffective = (& $e2eBinary --nwflash-effective-capabilities-probe 2>&1 | Out-String).Trim() | ConvertFrom-Json
    $effectiveCapabilities = @($e2eEffective.capabilities)
    if ($LASTEXITCODE -ne 0 -or $effectiveCapabilities.Count -ne 1 -or
        [string]$effectiveCapabilities[0].identifier -ne 'e2e') {
        throw 'Built E2E context does not select exactly the inline e2e capability.'
    }
    foreach ($permission in @('wdio:default', 'wdio-webdriver:default')) {
        if (@($effectiveCapabilities[0].permissions) -notcontains $permission) {
            throw "Built E2E context is missing $permission."
        }
    }
    Write-Host 'Generated production/E2E capability and dependency boundaries passed.'
}
finally {
    if ($null -eq $priorTarget) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue } else { $env:CARGO_TARGET_DIR = $priorTarget }
    if ($null -eq $priorBuildId) { Remove-Item Env:NWFLASH_BUILD_ID -ErrorAction SilentlyContinue } else { $env:NWFLASH_BUILD_ID = $priorBuildId }
    foreach ($entry in @(
        @{ Root = $productionTarget; Prefix = 'nwflash-capability-prod-' },
        @{ Root = $e2eTarget; Prefix = 'nwflash-capability-e2e-' }
    )) {
        if (Test-Path -LiteralPath $entry.Root) { Remove-ValidatedTemporaryRoot -Root $entry.Root -Prefix $entry.Prefix }
    }
}
