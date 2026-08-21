[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ReleaseRoot,
    [string]$ResourceManifestPath
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ResourceManifestPath)) {
    $ResourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
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

function Get-ReleaseRelativePath {
    param([Parameter(Mandatory = $true)][string]$Root, [Parameter(Mandatory = $true)][string]$Path)

    return (Get-SafeRelativePath ([IO.Path]::GetRelativePath($Root, $Path)))
}

function Get-ResourceEntries {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Resource allowlist is missing: $Path"
    }
    $document = Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
    if ($null -eq $document.resources) {
        throw 'Resource allowlist has no resources array.'
    }
    $entries = @($document.resources)
    if ($entries.Count -eq 0) {
        throw 'Resource allowlist cannot be empty.'
    }
    return $entries
}

if (-not (Test-Path -LiteralPath $ReleaseRoot -PathType Container)) {
    throw "Release root is missing: $ReleaseRoot"
}
$root = [IO.Path]::GetFullPath($ReleaseRoot)
$entries = Get-ResourceEntries $ResourceManifestPath
$allowed = @{}

foreach ($entry in $entries) {
    if ($null -eq $entry.destination -or $null -eq $entry.sha256) {
        throw 'Resource allowlist entry is incomplete.'
    }
    $destination = Get-SafeRelativePath ([string]$entry.destination)
    $relative = "resources/$destination"
    if ($allowed.ContainsKey($relative)) {
        throw "Resource allowlist has a duplicate destination: $relative"
    }
    $path = Join-Path $root $relative
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required bundled resource is missing: $relative"
    }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    if (-not $hash.Equals(([string]$entry.sha256), [StringComparison]::OrdinalIgnoreCase)) {
        throw "Bundled resource integrity mismatch: $relative"
    }
    $allowed[$relative] = $hash.ToUpperInvariant()
}

$executable = Join-Path $root 'nwflash-desktop.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw 'Release executable is missing.'
}
$installers = @(Get-ChildItem -LiteralPath $root -File -Filter '*-setup.exe' -ErrorAction SilentlyContinue)
if ($installers.Count -ne 1) {
    throw 'Release NSIS installer is missing or ambiguous.'
}

$allowed['nwflash-desktop.exe'] = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToUpperInvariant()
$allowed[$installers[0].Name] = (Get-FileHash -LiteralPath $installers[0].FullName -Algorithm SHA256).Hash.ToUpperInvariant()
$ignored = @('SHA256SUMS.txt', '.nwflash-tauri-release')

foreach ($file in @(Get-ChildItem -LiteralPath $root -Recurse -File -Force)) {
    $relative = Get-ReleaseRelativePath $root $file.FullName
    if ($ignored -contains $relative) {
        continue
    }
    if (-not $allowed.ContainsKey($relative)) {
        throw "Unexpected release artifact: $relative"
    }
}

$manifest = Join-Path $root 'SHA256SUMS.txt'
$lines = @($allowed.Keys | Sort-Object | ForEach-Object { '{0} *{1}' -f $allowed[$_], $_ })
Set-Content -LiteralPath $manifest -Value $lines -Encoding UTF8
Write-Host 'Tauri release manifest created.'
