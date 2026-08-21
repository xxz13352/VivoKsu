[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [string]$InstallRoot,
    [string]$ResourceManifestPath
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ResourceManifestPath)) {
    $ResourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
}
if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
    throw "NSIS installer is missing: $InstallerPath"
}
if (-not (Test-Path -LiteralPath $ResourceManifestPath -PathType Leaf)) {
    throw "Resource allowlist is missing: $ResourceManifestPath"
}
if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
    $InstallRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-installer-" + [Guid]::NewGuid().ToString('N'))
}
$installRootPath = [IO.Path]::GetFullPath($InstallRoot)

try {
    $installation = Start-Process -FilePath $InstallerPath -ArgumentList @('/S', "/D=$installRootPath") -Wait -PassThru
    if ($installation.ExitCode -ne 0) {
        throw "NSIS installation failed with exit code $($installation.ExitCode)."
    }
    if (-not (Test-Path -LiteralPath (Join-Path $installRootPath 'nwflash-desktop.exe') -PathType Leaf)) {
        throw 'Installed Tauri executable is missing.'
    }

    $resources = (Get-Content -Raw -LiteralPath $ResourceManifestPath | ConvertFrom-Json).resources
    foreach ($entry in @($resources)) {
        $relative = ([string]$entry.destination).Replace('/', '\')
        if (-not (Test-Path -LiteralPath (Join-Path $installRootPath (Join-Path 'resources' $relative)) -PathType Leaf)) {
            throw "Installed required resource is missing: resources\\$relative"
        }
    }

    $uninstaller = Join-Path $installRootPath 'uninstall.exe'
    if (-not (Test-Path -LiteralPath $uninstaller -PathType Leaf)) {
        throw 'NSIS uninstaller is missing.'
    }
    $uninstallation = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
    if ($uninstallation.ExitCode -ne 0) {
        throw "NSIS uninstallation failed with exit code $($uninstallation.ExitCode)."
    }
    if (Test-Path -LiteralPath $installRootPath) {
        throw 'NSIS uninstaller left the installation directory behind.'
    }
} finally {
    if (Test-Path -LiteralPath $installRootPath) {
        Remove-Item -LiteralPath $installRootPath -Recurse -Force
    }
}

Write-Host 'Tauri installer install/uninstall verification passed.'
