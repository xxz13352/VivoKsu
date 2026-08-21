[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

function Assert-ReleaseCondition {
    param(
        [bool]$Condition,
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot 'src\Nwflash.Desktop\src-tauri'
$configPath = Join-Path $tauriRoot 'tauri.conf.json'
$config = Get-Content -Raw $configPath | ConvertFrom-Json

Assert-ReleaseCondition ($config.bundle.targets -contains 'nsis') 'Tauri release must declare the NSIS target.'
Assert-ReleaseCondition ($config.bundle.windows.nsis.installMode -eq 'currentUser') 'NSIS must install per-user.'
Assert-ReleaseCondition ($config.bundle.windows.webviewInstallMode.type -eq 'embedBootstrapper') 'NSIS must embed the WebView2 bootstrapper.'
Assert-ReleaseCondition ($config.bundle.windows.nsis.languages -contains 'SimpChinese') 'NSIS must include Simplified Chinese installer messages.'

$expectedResources = @(
    'resources/platform-tools/adb.exe',
    'resources/platform-tools/AdbWinApi.dll',
    'resources/platform-tools/AdbWinUsbApi.dll',
    'resources/platform-tools/fastboot.exe',
    'resources/platform-tools/PLATFORM_TOOLS.SHA256',
    'resources/drivers/vivo-usb-driver.7z',
    'resources/root-tools/magiskboot.so'
)
$configuredResources = @($config.bundle.resources)
Assert-ReleaseCondition ($configuredResources.Count -eq $expectedResources.Count) 'Tauri bundle resources must be an exact allowlist.'
foreach ($resource in $expectedResources) {
    Assert-ReleaseCondition ($configuredResources -contains $resource) "Tauri bundle resource is missing from the allowlist: $resource"
}
Assert-ReleaseCondition (-not ($configuredResources | Where-Object { $_.Contains('*') })) 'Tauri bundle resources must not contain glob patterns.'

foreach ($scriptName in @('Publish-TauriRelease.ps1', 'Verify-TauriRelease.ps1')) {
    Assert-ReleaseCondition (Test-Path (Join-Path $PSScriptRoot $scriptName)) "Missing release script: $scriptName"
}

$resourcesReadme = Join-Path $tauriRoot 'resources\README.md'
Assert-ReleaseCondition (Test-Path $resourcesReadme) 'Tauri bundled-resource inventory is missing.'
$inventory = Get-Content -Raw $resourcesReadme
foreach ($required in @('platform-tools', 'drivers', 'root-tools', 'scrcpy', 'payload_dumper', 'APK')) {
    Assert-ReleaseCondition ($inventory.Contains($required)) "Bundled-resource inventory does not document $required."
}

foreach ($document in @('README.md', 'docs\architecture.md', 'docs\architecture-tauri-migration.md')) {
    $content = Get-Content -Raw (Join-Path $repoRoot $document)
    Assert-ReleaseCondition ($content.Contains('Verify-TauriRelease.ps1')) "$document does not document the Tauri release verifier."
}

$unmarkedRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-unmarked-test-" + [Guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Force $unmarkedRoot | Out-Null
    Set-Content -Encoding ASCII (Join-Path $unmarkedRoot 'user-file.txt') 'must not be deleted'
    $unmarkedRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -SkipBuild -DevelopmentUnsigned -ReleaseRoot $unmarkedRoot
    } catch {
        $unmarkedRejected = $_.Exception.Message -like '*not an NWflash Tauri staging directory*'
    }
    Assert-ReleaseCondition $unmarkedRejected 'Publish script must reject an unmarked nonempty release root.'
    Assert-ReleaseCondition (Test-Path (Join-Path $unmarkedRoot 'user-file.txt')) 'Publish script removed data from an unmarked release root.'
} finally {
    if (Test-Path $unmarkedRoot) {
        Remove-Item -LiteralPath $unmarkedRoot -Recurse -Force
    }
}

$releaseRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-release-test-" + [Guid]::NewGuid().ToString('N'))
try {
    & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -DevelopmentUnsigned -ReleaseRoot $releaseRoot
    & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -SkipBuild -DevelopmentUnsigned -ReleaseRoot $releaseRoot

    Assert-ReleaseCondition (Test-Path (Join-Path $releaseRoot 'nwflash-desktop.exe')) 'Staged release executable is missing.'
    Assert-ReleaseCondition ((Get-ChildItem $releaseRoot -File -Filter '*-setup.exe').Count -eq 1) 'Staged NSIS installer is missing.'
    foreach ($resource in @('resources\platform-tools\adb.exe', 'resources\platform-tools\fastboot.exe', 'resources\drivers\vivo-usb-driver.7z', 'resources\root-tools\magiskboot.so')) {
        Assert-ReleaseCondition (Test-Path (Join-Path $releaseRoot $resource)) "Staged required resource is missing: $resource"
    }
    Assert-ReleaseCondition (Test-Path (Join-Path $releaseRoot 'SHA256SUMS.txt')) 'Staged SHA-256 manifest is missing.'
    $installerName = (Get-ChildItem $releaseRoot -File -Filter '*-setup.exe').Name
    Assert-ReleaseCondition ((Get-Content -Raw (Join-Path $releaseRoot 'SHA256SUMS.txt')).Contains("*$installerName")) 'SHA-256 manifest must preserve the NSIS installer filename.'

    $installRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-install-test-" + [Guid]::NewGuid().ToString('N'))
    try {
        $installer = Get-ChildItem $releaseRoot -File -Filter '*-setup.exe'
        $installation = Start-Process -FilePath $installer.FullName -ArgumentList @('/S', "/D=$installRoot") -Wait -PassThru
        Assert-ReleaseCondition ($installation.ExitCode -eq 0) 'NSIS installation failed.'
        $installedApp = Join-Path $installRoot 'nwflash-desktop.exe'
        Assert-ReleaseCondition (Test-Path $installedApp) 'Installed Tauri executable is missing.'
        foreach ($resource in @('resources\platform-tools\adb.exe', 'resources\platform-tools\fastboot.exe', 'resources\drivers\vivo-usb-driver.7z', 'resources\root-tools\magiskboot.so')) {
            Assert-ReleaseCondition (Test-Path (Join-Path $installRoot $resource)) "Installed required resource is missing: $resource"
        }

        $uninstaller = Join-Path $installRoot 'uninstall.exe'
        Assert-ReleaseCondition (Test-Path $uninstaller) 'NSIS uninstaller is missing.'
        $uninstallation = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru
        Assert-ReleaseCondition ($uninstallation.ExitCode -eq 0) 'NSIS uninstallation failed.'
        Assert-ReleaseCondition (-not (Test-Path $installRoot)) 'NSIS uninstaller left the installation directory behind.'
    } finally {
        if (Test-Path $installRoot) {
            Remove-Item -LiteralPath $installRoot -Recurse -Force
        }
    }

    Set-Content -Encoding ASCII (Join-Path $releaseRoot 'resources\scrcpy.exe') 'test-only forbidden resource'
    $forbiddenRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $releaseRoot
    } catch {
        $forbiddenRejected = $_.Exception.Message -like '*On-demand resource was bundled*'
    }
    Assert-ReleaseCondition $forbiddenRejected 'Release verifier must reject on-demand resources.'
} finally {
    if (Test-Path $releaseRoot) {
        Remove-Item -LiteralPath $releaseRoot -Recurse -Force
    }
}

$isolatedCargoTarget = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-isolated-target-" + [Guid]::NewGuid().ToString('N'))
$isolatedReleaseRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-isolated-release-" + [Guid]::NewGuid().ToString('N'))
$priorCargoTargetDir = $env:CARGO_TARGET_DIR
try {
    $isolatedReleaseDirectory = Join-Path $isolatedCargoTarget 'release'
    $isolatedResources = Join-Path $isolatedReleaseDirectory 'resources'
    New-Item -ItemType Directory -Force (Join-Path $isolatedReleaseDirectory 'bundle\nsis') | Out-Null
    Set-Content -Encoding ASCII -LiteralPath (Join-Path $isolatedReleaseDirectory 'nwflash-desktop.exe') -Value 'isolated executable source'
    $resourceManifest = Get-Content -Raw (Join-Path $repoRoot 'packaging\release\tauri-resources.json') | ConvertFrom-Json
    foreach ($resource in @($resourceManifest.resources)) {
        $source = Join-Path $repoRoot ([string]$resource.source).Replace('/', '\')
        $destination = Join-Path $isolatedResources ([string]$resource.destination).Replace('/', '\')
        New-Item -ItemType Directory -Force (Split-Path -Parent $destination) | Out-Null
        Copy-Item -LiteralPath $source -Destination $destination -Force
    }
    Set-Content -Encoding ASCII -LiteralPath (Join-Path $isolatedReleaseDirectory 'bundle\nsis\isolated-setup.exe') -Value 'isolated installer source'

    $env:CARGO_TARGET_DIR = $isolatedCargoTarget
    & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -SkipBuild -DevelopmentUnsigned -ReleaseRoot $isolatedReleaseRoot

    Assert-ReleaseCondition (
        (Get-Content -Raw (Join-Path $isolatedReleaseRoot 'nwflash-desktop.exe')) -eq
        (Get-Content -Raw (Join-Path $isolatedReleaseDirectory 'nwflash-desktop.exe'))
    ) 'Publish script must stage the executable from CARGO_TARGET_DIR.'
    Assert-ReleaseCondition (
        (Get-Content -Raw (Join-Path $isolatedReleaseRoot 'isolated-setup.exe')) -eq
        (Get-Content -Raw (Join-Path $isolatedReleaseDirectory 'bundle\nsis\isolated-setup.exe'))
    ) 'Publish script must stage the NSIS installer from CARGO_TARGET_DIR.'
    $sourceAdbHash = (Get-FileHash -LiteralPath (Join-Path $isolatedResources 'platform-tools\adb.exe') -Algorithm SHA256).Hash
    $stagedAdbHash = (Get-FileHash -LiteralPath (Join-Path $isolatedReleaseRoot 'resources\platform-tools\adb.exe') -Algorithm SHA256).Hash
    Assert-ReleaseCondition ($stagedAdbHash -eq $sourceAdbHash) 'Publish script must stage allowlisted resources from CARGO_TARGET_DIR.'
} finally {
    if ($null -eq $priorCargoTargetDir) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue } else { $env:CARGO_TARGET_DIR = $priorCargoTargetDir }
    if (Test-Path $isolatedCargoTarget) { Remove-Item -LiteralPath $isolatedCargoTarget -Recurse -Force }
    if (Test-Path $isolatedReleaseRoot) { Remove-Item -LiteralPath $isolatedReleaseRoot -Recurse -Force }
}

Write-Host 'Tauri release contract passed.'
