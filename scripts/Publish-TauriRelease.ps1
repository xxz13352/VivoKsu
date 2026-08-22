[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$DevelopmentUnsigned,
    [string]$ReleaseRoot
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$desktop = Join-Path $repo 'src\Nwflash.Desktop'
$tauri = Join-Path $desktop 'src-tauri'
$resources = Join-Path $tauri 'resources'
$resourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'

if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $cargoTargetDirectory = Join-Path $tauri 'target'
} elseif ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
    $cargoTargetDirectory = $env:CARGO_TARGET_DIR
} else {
    $cargoTargetDirectory = Join-Path $repo $env:CARGO_TARGET_DIR
}
$cargoTargetDirectory = [IO.Path]::GetFullPath($cargoTargetDirectory)
$env:CARGO_TARGET_DIR = $cargoTargetDirectory

function Invoke-CheckedExternalCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Description,
        [Parameter(Mandatory = $true)][scriptblock]$Command
    )

    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Description failed with exit code $LASTEXITCODE."
    }
}

function Get-SafeRelativePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    $normalized = $Path.Replace('\', '/').Trim('/')
    if ([string]::IsNullOrWhiteSpace($normalized) -or [IO.Path]::IsPathRooted($Path)) {
        throw "Release resource path is invalid: $Path"
    }
    foreach ($segment in $normalized.Split('/')) {
        if ([string]::IsNullOrWhiteSpace($segment) -or $segment -eq '.' -or $segment -eq '..') {
            throw "Release resource path is invalid: $Path"
        }
    }
    return $normalized
}

function Get-ResourceEntries {
    if (-not (Test-Path -LiteralPath $resourceManifestPath -PathType Leaf)) {
        throw "Resource allowlist is missing: $resourceManifestPath"
    }

    $document = Get-Content -Raw -LiteralPath $resourceManifestPath | ConvertFrom-Json
    $entries = @($document.resources)
    if ($entries.Count -eq 0) {
        throw 'Resource allowlist cannot be empty.'
    }

    $destinations = @{}
    foreach ($entry in $entries) {
        if ($null -eq $entry.source -or $null -eq $entry.destination -or $null -eq $entry.sha256) {
            throw 'Resource allowlist entry is incomplete.'
        }
        $source = Get-SafeRelativePath ([string]$entry.source)
        $destination = Get-SafeRelativePath ([string]$entry.destination)
        $digest = ([string]$entry.sha256).Trim()
        if ($digest -notmatch '^[0-9a-fA-F]{64}$') {
            throw "Resource allowlist digest is invalid: $destination"
        }
        if ($destinations.ContainsKey($destination)) {
            throw "Resource allowlist has a duplicate destination: $destination"
        }
        $destinations[$destination] = $true
        $entry.source = $source
        $entry.destination = $destination
        $entry.sha256 = $digest
    }

    return $entries
}

function Get-RepositoryFilePath {
    param([Parameter(Mandatory = $true)][string]$RelativePath)

    $path = [IO.Path]::GetFullPath((Join-Path $repo $RelativePath))
    $repoPrefix = $repo.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if (-not $path.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Resource source escapes the repository: $RelativePath"
    }
    return $path
}

function Get-ResourceDestinationPath {
    param([Parameter(Mandatory = $true)][string]$Root, [Parameter(Mandatory = $true)][string]$RelativePath)

    $path = [IO.Path]::GetFullPath((Join-Path $Root $RelativePath.Replace('/', '\')))
    $rootPrefix = ([IO.Path]::GetFullPath($Root)).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if (-not $path.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Resource destination escapes its root: $RelativePath"
    }
    return $path
}

function Assert-FileHash {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$ExpectedHash,
        [Parameter(Mandatory = $true)][string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description is missing: $Path"
    }
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if (-not $actual.Equals($ExpectedHash, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description integrity mismatch: $Path"
    }
}

function Assert-ExpectedResourceTree {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][object[]]$Entries,
        [Parameter(Mandatory = $true)][string]$Description,
        [string[]]$IgnoredPaths = @()
    )

    $expected = @{}
    foreach ($entry in $Entries) {
        $relative = ([string]$entry.destination).Replace('\', '/')
        $path = Get-ResourceDestinationPath -Root $Root -RelativePath $relative
        Assert-FileHash -Path $path -ExpectedHash ([string]$entry.sha256) -Description $Description
        $expected[$relative] = $true
    }

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        throw "$Description root is missing: $Root"
    }
    foreach ($file in @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force)) {
        $rootPath = ([IO.Path]::GetFullPath($Root)).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
        $relative = [Uri]::UnescapeDataString(([Uri]$rootPath).MakeRelativeUri([Uri]([IO.Path]::GetFullPath($file.FullName))).ToString()).Replace('/', '/')
        if ($relative.StartsWith('../') -or $relative -eq '..') {
            throw "Path escapes root: $($file.FullName)"
        }
        if ($IgnoredPaths -contains $relative) {
            continue
        }
        if (-not $expected.ContainsKey($relative)) {
            throw "Unexpected ${Description}: $relative"
        }
    }
}

function Stage-BundledResources {
    param([Parameter(Mandatory = $true)][object[]]$Entries)

    $resourceRoot = ([IO.Path]::GetFullPath($resources)).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $resolvedEntries = @(
        foreach ($entry in $Entries) {
            [pscustomobject]@{
                Entry = $entry
                Source = Get-RepositoryFilePath ([string]$entry.source)
                Destination = Get-ResourceDestinationPath -Root $resources -RelativePath ([string]$entry.destination)
            }
        }
    )

    $allSourcesMatchDestinations = @($resolvedEntries | Where-Object {
        -not $_.Source.Equals($_.Destination, [StringComparison]::OrdinalIgnoreCase)
    }).Count -eq 0
    if ($allSourcesMatchDestinations) {
        Assert-ExpectedResourceTree -Root $resources -Entries $Entries -Description 'active Tauri resource' -IgnoredPaths @('README.md')
        return
    }

    foreach ($resolved in $resolvedEntries) {
        if ($resolved.Source.StartsWith($resourceRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Release resource source overlaps Tauri staging: $($resolved.Entry.source)"
        }
    }

    $topLevelDirectories = @($Entries | ForEach-Object { ([string]$_.destination).Split('/')[0] } | Select-Object -Unique)
    foreach ($directoryName in $topLevelDirectories) {
        $directory = Get-ResourceDestinationPath -Root $resources -RelativePath $directoryName
        if (Test-Path -LiteralPath $directory) {
            Remove-Item -LiteralPath $directory -Recurse -Force
        }
    }

    foreach ($resolved in $resolvedEntries) {
        Assert-FileHash -Path $resolved.Source -ExpectedHash ([string]$resolved.Entry.sha256) -Description 'Release resource source'
        New-Item -ItemType Directory -Force (Split-Path -Parent $resolved.Destination) | Out-Null
        Copy-Item -LiteralPath $resolved.Source -Destination $resolved.Destination -Force
        Assert-FileHash -Path $resolved.Destination -ExpectedHash ([string]$resolved.Entry.sha256) -Description 'Staged Tauri resource'
    }

    Assert-ExpectedResourceTree -Root $resources -Entries $Entries -Description 'staged Tauri resource' -IgnoredPaths @('README.md')
}

function Copy-ReleaseResources {
    param(
        [Parameter(Mandatory = $true)][string]$SourceRoot,
        [Parameter(Mandatory = $true)][string]$DestinationRoot,
        [Parameter(Mandatory = $true)][object[]]$Entries
    )

    foreach ($entry in $Entries) {
        $relative = [string]$entry.destination
        $source = Get-ResourceDestinationPath -Root $SourceRoot -RelativePath $relative
        Assert-FileHash -Path $source -ExpectedHash ([string]$entry.sha256) -Description 'Tauri build resource'
        $destination = Get-ResourceDestinationPath -Root $DestinationRoot -RelativePath $relative
        New-Item -ItemType Directory -Force (Split-Path -Parent $destination) | Out-Null
        Copy-Item -LiteralPath $source -Destination $destination -Force
    }
    Assert-ExpectedResourceTree -Root $DestinationRoot -Entries $Entries -Description 'release resource'
}

function Assert-ProtectedReleasePrerequisites {
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_PATH)) {
        throw 'NWFLASH_VMP_PATH is required for a protected release.'
    }
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_PROJECT)) {
        throw 'NWFLASH_VMP_PROJECT is required for a protected release.'
    }
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_ARGUMENTS)) {
        throw 'NWFLASH_VMP_ARGUMENTS is required for a protected release.'
    }
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_CERT_THUMBPRINT)) {
        throw 'NWFLASH_CERT_THUMBPRINT is required for a protected release.'
    }
    if (-not (Test-Path -LiteralPath $env:NWFLASH_VMP_PATH -PathType Leaf)) {
        throw "VMProtect executable is missing: $env:NWFLASH_VMP_PATH"
    }

    $project = [IO.Path]::GetFullPath($env:NWFLASH_VMP_PROJECT)
    if (-not (Test-Path -LiteralPath $project -PathType Leaf)) {
        throw "VMProtect project is missing: $project"
    }
    $repoPrefix = $repo.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if ($project.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'NWFLASH_VMP_PROJECT must reference an externally controlled VMProtect project.'
    }

    try {
        $arguments = ConvertFrom-Json $env:NWFLASH_VMP_ARGUMENTS
    } catch {
        throw 'NWFLASH_VMP_ARGUMENTS must be a JSON argument array.'
    }
    if ($arguments -isnot [Array] -or @($arguments).Count -eq 0 -or @($arguments | Where-Object { $_ -isnot [string] }).Count -ne 0) {
        throw 'NWFLASH_VMP_ARGUMENTS must be a non-empty JSON array of strings.'
    }
    foreach ($placeholder in @('{project}', '{input}', '{output}')) {
        if (-not (@($arguments | Where-Object { $_.Contains($placeholder) }))) {
            throw "NWFLASH_VMP_ARGUMENTS must contain the $placeholder placeholder."
        }
    }

    $thumbprint = ($env:NWFLASH_CERT_THUMBPRINT -replace '\s', '').ToUpperInvariant()
    if ($thumbprint -notmatch '^[0-9A-F]{40}$') {
        throw 'NWFLASH_CERT_THUMBPRINT must be a 40-character SHA-1 certificate thumbprint.'
    }
}

function Initialize-ReleaseRoot {
    param([Parameter(Mandatory = $true)][string]$Root)

    $marker = Join-Path $Root '.nwflash-tauri-release'
    if (Test-Path -LiteralPath $Root) {
        if (Get-ChildItem -LiteralPath $Root -Force | Select-Object -First 1) {
            if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
                throw "Release root is not an NWflash Tauri staging directory: $Root"
            }
            Get-ChildItem -LiteralPath $Root -Force | Remove-Item -Recurse -Force
        }
    } else {
        New-Item -ItemType Directory -Force $Root | Out-Null
    }
    Set-Content -Encoding ASCII -LiteralPath $marker -Value 'NWflash Tauri release staging directory.'
}

if ([string]::IsNullOrWhiteSpace($ReleaseRoot)) {
    $ReleaseRoot = Join-Path $repo 'artifacts\tauri-release'
}
$releaseRootPath = [IO.Path]::GetFullPath($ReleaseRoot)
$resourceEntries = @(Get-ResourceEntries)

if (-not $DevelopmentUnsigned) {
    Assert-ProtectedReleasePrerequisites
    Invoke-CheckedExternalCommand -Description 'Rust workspace tests' -Command { cargo test --manifest-path (Join-Path $tauri 'Cargo.toml') --workspace --all-targets }
    Invoke-CheckedExternalCommand -Description 'Rust formatting check' -Command { cargo fmt --manifest-path (Join-Path $tauri 'Cargo.toml') --all -- --check }
    Invoke-CheckedExternalCommand -Description 'Rust clippy check' -Command { cargo clippy --manifest-path (Join-Path $tauri 'Cargo.toml') --workspace --all-targets -- -D warnings }
    Invoke-CheckedExternalCommand -Description 'Frontend tests' -Command { npm run test --prefix $desktop }
    Invoke-CheckedExternalCommand -Description 'Native Tauri E2E tests' -Command { npm run test:native --prefix (Join-Path $desktop 'e2e-tests') }
}

Initialize-ReleaseRoot -Root $releaseRootPath
Stage-BundledResources -Entries $resourceEntries

if (-not $SkipBuild) {
    Invoke-CheckedExternalCommand -Description 'Frontend production build' -Command { npm run build --prefix $desktop }
    if ($DevelopmentUnsigned) {
        Invoke-CheckedExternalCommand -Description 'Development Tauri production build' -Command { npm run tauri --prefix $desktop -- build --no-sign }
    } else {
        Invoke-CheckedExternalCommand -Description 'Unbundled Tauri production build' -Command { npm run tauri --prefix $desktop -- build --no-sign --no-bundle }
    }
}

$releaseDirectory = Join-Path $cargoTargetDirectory 'release'
$executable = Join-Path $releaseDirectory 'nwflash-desktop.exe'
$bundleDirectory = Join-Path $releaseDirectory 'bundle\nsis'
$stagedResources = Join-Path $releaseDirectory 'resources'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Tauri release executable is missing: $executable"
}

Copy-Item -LiteralPath $executable -Destination (Join-Path $releaseRootPath 'nwflash-desktop.exe') -Force
Copy-ReleaseResources -SourceRoot $stagedResources -DestinationRoot (Join-Path $releaseRootPath 'resources') -Entries $resourceEntries

if (-not $DevelopmentUnsigned) {
    & (Join-Path $PSScriptRoot 'Protect-NwflashRelease.ps1') -ReleaseRoot $releaseRootPath
    & (Join-Path $PSScriptRoot 'Sign-NwflashRelease.ps1') -Path (Join-Path $releaseRootPath 'nwflash-desktop.exe')
    Copy-Item -LiteralPath (Join-Path $releaseRootPath 'nwflash-desktop.exe') -Destination $executable -Force
    Invoke-CheckedExternalCommand -Description 'NSIS bundle build' -Command { npm run tauri --prefix $desktop -- bundle --no-sign }
}

$installers = @(Get-ChildItem -LiteralPath $bundleDirectory -File -Filter '*-setup.exe' -ErrorAction SilentlyContinue)
if ($installers.Count -ne 1) {
    throw "Expected exactly one NSIS installer in: $bundleDirectory"
}
if (-not $DevelopmentUnsigned) {
    & (Join-Path $PSScriptRoot 'Sign-NwflashRelease.ps1') -Path $installers[0].FullName
}
Copy-Item -LiteralPath $installers[0].FullName -Destination (Join-Path $releaseRootPath $installers[0].Name) -Force

& (Join-Path $PSScriptRoot 'New-TauriReleaseManifest.ps1') -ReleaseRoot $releaseRootPath -ResourceManifestPath $resourceManifestPath
if ($DevelopmentUnsigned) {
    & (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $releaseRootPath
    Write-Host 'Development unsigned Tauri release staging completed.'
} else {
    & (Join-Path $PSScriptRoot 'Verify-ProtectedRelease.ps1') -ReleaseRoot $releaseRootPath
    & (Join-Path $PSScriptRoot 'Test-TauriInstaller.ps1') -InstallerPath $installers[0].FullName -ResourceManifestPath $resourceManifestPath
    Write-Host 'Protected and signed Tauri release staging completed.'
}
