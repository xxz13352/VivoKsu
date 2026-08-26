[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$tauri = Join-Path $repo 'src\Nwflash.Desktop\src-tauri'
$resourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-ThrowsLike {
    param([scriptblock]$Action, [string]$Pattern, [string]$Message)
    $rejected = $false
    try { & $Action } catch { $rejected = $_.Exception.Message -like $Pattern }
    Assert-Condition $rejected $Message
}

$config = Get-Content -Raw -LiteralPath (Join-Path $tauri 'tauri.conf.json') | ConvertFrom-Json
$targets = @($config.bundle.targets)
$capabilities = @($config.app.security.capabilities)
Assert-Condition ($targets.Count -eq 1 -and $targets[0] -ceq 'nsis') 'Tauri release target must be exactly NSIS.'
Assert-Condition ($config.bundle.windows.nsis.installMode -eq 'currentUser') 'NSIS must install per-user.'
Assert-Condition ($config.bundle.windows.webviewInstallMode.type -eq 'embedBootstrapper') 'NSIS must embed the WebView2 bootstrapper.'
Assert-Condition ($capabilities.Count -eq 1 -and $capabilities[0] -ceq 'default') 'Production Tauri config must select only default.'
Assert-Condition (-not (@($config.bundle.resources) | Where-Object { $_ -match '[*?]' })) 'Tauri bundle resources must not use globs.'

$unmarkedRoot = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-tauri-unmarked-' + [Guid]::NewGuid().ToString('N'))
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-tauri-fixture-' + [Guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $unmarkedRoot | Out-Null
    [IO.File]::WriteAllText((Join-Path $unmarkedRoot 'user-file.txt'), 'must remain')
    Assert-ThrowsLike {
        & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -DevelopmentUnsigned -SkipBuild -DevelopmentReleaseRoot $unmarkedRoot
    } '*fresh and empty*' 'Development publisher accepted a nonempty root.'
    Assert-Condition (Test-Path -LiteralPath (Join-Path $unmarkedRoot 'user-file.txt')) 'Publisher removed an unrelated file.'

    New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'resources') | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $fixtureRoot 'nwflash-desktop.exe'), [byte[]](1, 2, 3, 4))
    [IO.File]::WriteAllBytes((Join-Path $fixtureRoot 'fixture-setup.exe'), [byte[]](5, 6, 7, 8))
    [IO.File]::WriteAllText((Join-Path $fixtureRoot '.nwflash-tauri-release'), 'fixture')
    $resources = @((Get-Content -Raw -LiteralPath $resourceManifestPath | ConvertFrom-Json).resources)
    foreach ($entry in $resources) {
        $source = Join-Path $repo ([string]$entry.source).Replace('/', '\')
        $target = Join-Path $fixtureRoot ('resources\' + ([string]$entry.destination).Replace('/', '\'))
        $parent = Split-Path -Parent $target
        if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent | Out-Null }
        Copy-Item -LiteralPath $source -Destination $target
    }
    & (Join-Path $PSScriptRoot 'New-TauriReleaseManifest.ps1') -ReleaseRoot $fixtureRoot -ResourceManifestPath $resourceManifestPath -DevelopmentUnsigned
    & (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $fixtureRoot -ResourceManifestPath $resourceManifestPath

    $manifestEntries = @{}
    foreach ($line in Get-Content -LiteralPath (Join-Path $fixtureRoot 'SHA256SUMS.txt')) {
        if ($line -notmatch '^(?<hash>[0-9A-F]{64}) \*(?<path>.+)$') { throw 'Generated manifest has an invalid line.' }
        $manifestEntries[$Matches.path] = $Matches.hash
    }
    $expectedKeys = @('nwflash-desktop.exe', 'fixture-setup.exe') + @($resources | ForEach-Object { 'resources/' + ([string]$_.destination).Replace('\', '/') })
    Assert-Condition ($manifestEntries.Count -eq $expectedKeys.Count) 'Manifest does not contain the exact final file count.'
    foreach ($key in $expectedKeys) { Assert-Condition $manifestEntries.ContainsKey($key) "Manifest is missing exact path $key." }

    $forbidden = Join-Path $fixtureRoot 'debug.pdb'
    [IO.File]::WriteAllText($forbidden, 'forbidden')
    Assert-ThrowsLike {
        & (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $fixtureRoot -ResourceManifestPath $resourceManifestPath
    } '*Unexpected release artifact*' 'Release verifier accepted an unexpected PDB.'
    Write-Host 'Tauri release fixture contracts passed.'
}
finally {
    if (Test-Path -LiteralPath $unmarkedRoot) { Remove-ValidatedTemporaryRoot -Root $unmarkedRoot -Prefix 'nwflash-tauri-unmarked-' }
    if (Test-Path -LiteralPath $fixtureRoot) { Remove-ValidatedTemporaryRoot -Root $fixtureRoot -Prefix 'nwflash-tauri-fixture-' }
}
