[CmdletBinding()]
param(
    [switch]$DryRun,
    [string]$ReleaseRoot,
    [string]$ResourceManifestPath
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ReleaseRoot)) {
    $ReleaseRoot = Join-Path $repo 'artifacts\tauri-release'
}
if ([string]::IsNullOrWhiteSpace($ResourceManifestPath)) {
    $ResourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
}
$root = [IO.Path]::GetFullPath($ReleaseRoot)

if ($DryRun) {
    Write-Host "Dry-run release root: $root"
    exit 0
}

function Get-SafeRelativePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    $normalized = $Path.Replace('\', '/').Trim('/')
    if ([string]::IsNullOrWhiteSpace($normalized) -or [IO.Path]::IsPathRooted($Path)) {
        throw "Release manifest path is invalid: $Path"
    }
    foreach ($segment in $normalized.Split('/')) {
        if ([string]::IsNullOrWhiteSpace($segment) -or $segment -eq '.' -or $segment -eq '..') {
            throw "Release manifest path is invalid: $Path"
        }
    }
    return $normalized
}

function Get-ResourceEntries {
    if (-not (Test-Path -LiteralPath $ResourceManifestPath -PathType Leaf)) {
        throw "Resource allowlist is missing: $ResourceManifestPath"
    }
    $document = Get-Content -Raw -LiteralPath $ResourceManifestPath | ConvertFrom-Json
    $entries = @($document.resources)
    if ($entries.Count -eq 0) {
        throw 'Resource allowlist cannot be empty.'
    }
    return $entries
}

function Get-ReleaseManifest {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw 'Release SHA-256 manifest is missing.'
    }

    $entries = @{}
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line -notmatch '^(?<hash>[0-9A-Fa-f]{64}) \*(?<path>.+)$') {
            throw 'Release SHA-256 manifest contains an invalid entry.'
        }
        $relative = Get-SafeRelativePath $Matches.path
        if ($entries.ContainsKey($relative)) {
            throw "Release SHA-256 manifest contains a duplicate entry: $relative"
        }
        $entries[$relative] = $Matches.hash.ToUpperInvariant()
    }
    if ($entries.Count -eq 0) {
        throw 'Release SHA-256 manifest is empty.'
    }
    return $entries
}

function Get-ReleaseRelativePath {
    param([Parameter(Mandatory = $true)][string]$Path)
    $rootPath = $root.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $relative = [Uri]::UnescapeDataString(([Uri]$rootPath).MakeRelativeUri([Uri]([IO.Path]::GetFullPath($Path))).ToString())
    if ($relative.StartsWith('../') -or $relative -eq '..') {
        throw "Release file escapes root: $Path"
    }
    return $relative
}

if (-not (Test-Path -LiteralPath $root -PathType Container)) {
    throw "Release root is missing: $root"
}
$executable = Join-Path $root 'nwflash-desktop.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw 'Release executable is missing.'
}
$installers = @(Get-ChildItem -LiteralPath $root -File -Filter '*-setup.exe' -ErrorAction SilentlyContinue)
if ($installers.Count -ne 1) {
    throw 'Release NSIS installer is missing or ambiguous.'
}

foreach ($dotnetArtifact in @('*.runtimeconfig.json', '*.deps.json', 'hostfxr.dll', 'hostpolicy.dll', 'coreclr.dll')) {
    if (Get-ChildItem -LiteralPath $root -Recurse -File -Filter $dotnetArtifact) {
        throw "Release contains a .NET runtime artifact: $dotnetArtifact"
    }
}

$resourceEntries = @(Get-ResourceEntries)
$manifestPath = Join-Path $root 'SHA256SUMS.txt'
$manifest = Get-ReleaseManifest -Path $manifestPath
$expectedPaths = @{}
foreach ($entry in $resourceEntries) {
    if ($null -eq $entry.destination -or $null -eq $entry.sha256) {
        throw 'Resource allowlist entry is incomplete.'
    }
    $destination = Get-SafeRelativePath ([string]$entry.destination)
    $relative = "resources/$destination"
    if ($expectedPaths.ContainsKey($relative)) {
        throw "Resource allowlist has a duplicate destination: $relative"
    }
    $expectedPaths[$relative] = ([string]$entry.sha256).ToUpperInvariant()
}
$expectedPaths['nwflash-desktop.exe'] = $null
$expectedPaths[$installers[0].Name] = $null

if ($manifest.Count -ne $expectedPaths.Count) {
    throw 'Release SHA-256 manifest does not describe exactly the expected release files.'
}
foreach ($relative in $expectedPaths.Keys) {
    if (-not $manifest.ContainsKey($relative)) {
        throw "Release SHA-256 manifest is missing: $relative"
    }
    if ($null -ne $expectedPaths[$relative] -and $manifest[$relative] -ne $expectedPaths[$relative]) {
        throw "Release resource hash is not the approved hash: $relative"
    }
    $path = Join-Path $root $relative.Replace('/', '\')
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Release file is missing: $relative"
    }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($actual -ne $manifest[$relative]) {
        throw "Release SHA-256 mismatch: $relative"
    }
}

$ignored = @('SHA256SUMS.txt', '.nwflash-tauri-release')
foreach ($file in @(Get-ChildItem -LiteralPath $root -Recurse -File -Force)) {
    $relative = Get-ReleaseRelativePath -Path $file.FullName
    if ($ignored -contains $relative) {
        continue
    }
    if (-not $expectedPaths.ContainsKey($relative)) {
        throw "Unexpected release artifact: $relative"
    }
}

Write-Host 'Tauri release verification passed.'
