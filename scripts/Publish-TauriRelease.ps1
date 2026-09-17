#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding(DefaultParameterSetName = 'PrepareManual')]
param(
    [Parameter(Mandatory, ParameterSetName = 'PrepareManual')][switch]$PrepareManual,
    [Parameter(Mandatory, ParameterSetName = 'FinalizeManual')][switch]$FinalizeManual,
    [Parameter(Mandatory, ParameterSetName = 'Development')][switch]$DevelopmentUnsigned,
    [Parameter(Mandatory, ParameterSetName = 'PrepareManual')][string]$ProtectedOutputPath,
    [Parameter(Mandatory, ParameterSetName = 'PrepareManual')][string]$CompilerLogPath,
    [Parameter(Mandatory, ParameterSetName = 'PrepareManual')][string]$HandoffRoot,
    [Parameter(Mandatory, ParameterSetName = 'FinalizeManual')][string]$AcceptedEvidence,
    [Parameter(Mandatory, ParameterSetName = 'FinalizeManual')][string]$ReleaseRoot,
    [Parameter(ParameterSetName = 'Development')][string]$DevelopmentReleaseRoot,
    [Parameter(ParameterSetName = 'Development')][switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$desktop = Join-Path $repo 'src\Nwflash.Desktop'
$tauri = Join-Path $desktop 'src-tauri'
$resourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')

function Invoke-CheckedExternalCommand {
    param([Parameter(Mandatory)][string]$Description, [Parameter(Mandatory)][scriptblock]$Command)
    $global:LASTEXITCODE = 0
    & $Command
    $exitCode = $global:LASTEXITCODE
    if ($exitCode -ne 0) { throw "$Description failed with exit code $exitCode." }
}

function Get-ResourceEntries {
    $manifest = Resolve-FullyQualifiedLeaf $resourceManifestPath
    $entries = @((Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).resources)
    if ($entries.Count -eq 0) { throw 'Resource allowlist cannot be empty.' }
    $destinations = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $entries) {
        $source = ([string]$entry.source).Replace('\', '/').Trim('/')
        $destination = ([string]$entry.destination).Replace('\', '/').Trim('/')
        if ([string]::IsNullOrWhiteSpace($source) -or [string]::IsNullOrWhiteSpace($destination) -or
            [IO.Path]::IsPathFullyQualified($source) -or [IO.Path]::IsPathFullyQualified($destination) -or
            $source.Split('/') -contains '..' -or $destination.Split('/') -contains '..') {
            throw "Invalid release resource path: $source -> $destination"
        }
        if (-not $destinations.Add($destination)) { throw "Duplicate release resource destination: $destination" }
        if ([string]$entry.sha256 -notmatch '^[0-9A-Fa-f]{64}$') { throw "Invalid release resource SHA-256: $destination" }
    }
    $entries
}

function Copy-ApprovedResources {
    param([Parameter(Mandatory)][string]$DestinationRoot, [Parameter(Mandatory)][object[]]$Entries)
    $destination = Get-NormalizedFullPath $DestinationRoot
    if (-not (Test-Path -LiteralPath $destination)) { New-Item -ItemType Directory -Path $destination | Out-Null }
    $allowed = @()
    foreach ($entry in $Entries) {
        $source = Get-NormalizedFullPath (Join-Path $repo ([string]$entry.source).Replace('/', '\'))
        $source = Resolve-FullyQualifiedLeaf $source
        if ((Get-Sha256Hex $source) -ne ([string]$entry.sha256).ToUpperInvariant()) { throw "Approved resource source hash mismatch: $($entry.source)" }
        $relative = ([string]$entry.destination).Replace('/', '\')
        $target = Get-NormalizedFullPath (Join-Path $destination $relative)
        $prefix = $destination.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
        if (-not $target.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Resource destination escapes staging: $relative" }
        $parent = Split-Path -Parent $target
        if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent | Out-Null }
        Copy-Item -LiteralPath $source -Destination $target
        if ((Get-Sha256Hex $target) -ne ([string]$entry.sha256).ToUpperInvariant()) { throw "Staged resource hash mismatch: $relative" }
        $allowed += ([string]$entry.destination).Replace('\', '/')
    }
    Assert-ExactFileSet -Root $destination -AllowedRelativePaths $allowed
}

function Initialize-FreshRoot {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$MarkerName, [Parameter(Mandatory)][string]$MarkerValue)
    $fullPath = Get-NormalizedFullPath $Root
    Assert-NoReparseAncestors $fullPath
    if (Test-Path -LiteralPath $fullPath) {
        Assert-PathNotReparsePoint $fullPath | Out-Null
        if (Get-ChildItem -LiteralPath $fullPath -Force | Select-Object -First 1) { throw "Staging root must be fresh and empty: $fullPath" }
    }
    else { New-Item -ItemType Directory -Path $fullPath | Out-Null }
    [IO.File]::WriteAllText((Join-Path $fullPath $MarkerName), $MarkerValue, [Text.Encoding]::ASCII)
    $fullPath
}

function Resolve-CargoTargetRoot {
    if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) { return (Join-Path $tauri 'target') }
    if ([IO.Path]::IsPathFullyQualified($env:CARGO_TARGET_DIR)) { return (Get-NormalizedFullPath $env:CARGO_TARGET_DIR) }
    Get-NormalizedFullPath (Join-Path $repo $env:CARGO_TARGET_DIR)
}

$resourceEntries = @(Get-ResourceEntries)
Invoke-CheckedExternalCommand -Description 'PowerShell 7 runtime boundary' -Command {
    & (Join-Path $PSScriptRoot 'Test-PowerShellRuntimeBoundary.ps1')
}

if ($PSCmdlet.ParameterSetName -eq 'PrepareManual') {
    $protectedEnvironment = Assert-ProtectedBuildEnvironment
    Invoke-CheckedExternalCommand -Description 'Pinned VMProtect SDK identity' -Command {
        $sdkIdentity = & (Join-Path $PSScriptRoot 'vmp\verify-sdk.ps1') -SdkRoot $protectedEnvironment.sdk_root -AsJson | ConvertFrom-Json
        if (-not [bool]$sdkIdentity.verified -or [int]$sdkIdentity.files_copied -ne 0) {
            throw 'Pinned VMProtect SDK identity preflight failed.'
        }
    }
    Invoke-CheckedExternalCommand -Description 'VMProtect link and marker contracts' -Command {
        $layout = & (Join-Path $PSScriptRoot 'vmp\test-contracts.ps1') -SdkRoot $protectedEnvironment.sdk_root -AsJson | ConvertFrom-Json
        if (-not [bool]$layout.verified) { throw 'VMProtect link/marker preflight failed.' }
    }
    Invoke-CheckedExternalCommand -Description 'Frontend capability and Cargo graph tests' -Command {
        npm --prefix $desktop run test:capabilities
    }
    Invoke-CheckedExternalCommand -Description 'Rust protection probe tests' -Command {
        cargo test --manifest-path (Join-Path $tauri 'Cargo.toml') -p nwflash-protection --test vmp_probe
    }
    Invoke-CheckedExternalCommand -Description 'Rust desktop release-probe tests' -Command {
        cargo test --manifest-path (Join-Path $tauri 'Cargo.toml') -p nwflash-tauri --test release_probe
    }
    Invoke-CheckedExternalCommand -Description 'Frontend production build' -Command { npm --prefix $desktop run build }
    Invoke-CheckedExternalCommand -Description 'Protected unbundled Tauri build' -Command {
        npm --prefix $desktop run tauri -- build --features protected --no-sign --no-bundle
    }
    $targetRoot = Resolve-CargoTargetRoot
    $releaseDirectory = Join-Path $targetRoot 'release'
    $exe = Resolve-FullyQualifiedLeaf (Join-Path $releaseDirectory 'nwflash-desktop.exe')
    $pdb = Resolve-SingleProtectedDesktopPdb -ReleaseDirectory $releaseDirectory
    $map = Resolve-FullyQualifiedLeaf (Join-Path $releaseDirectory 'nwflash-desktop.map')
    & (Join-Path $PSScriptRoot 'vmp\prepare-manual-handoff.ps1') -InputExe $exe -InputPdb $pdb -InputMap $map `
        -ProtectedOutputPath $ProtectedOutputPath -CompilerLogPath $CompilerLogPath -HandoffRoot $HandoffRoot
    return
}

if ($PSCmdlet.ParameterSetName -eq 'Development') {
    if ([string]::IsNullOrWhiteSpace($DevelopmentReleaseRoot)) { $DevelopmentReleaseRoot = Join-Path $repo 'artifacts\tauri-release-development' }
    $stage = Initialize-FreshRoot -Root $DevelopmentReleaseRoot -MarkerName '.nwflash-tauri-release' -MarkerValue 'NWFlash development unsigned release.'
    if (-not $SkipBuild) {
        Invoke-CheckedExternalCommand -Description 'Frontend development release build' -Command { npm --prefix $desktop run build }
        Invoke-CheckedExternalCommand -Description 'Unsigned Tauri build' -Command { npm --prefix $desktop run tauri -- build --no-sign }
    }
    $targetRoot = Resolve-CargoTargetRoot
    $releaseDirectory = Join-Path $targetRoot 'release'
    $exe = Resolve-FullyQualifiedLeaf (Join-Path $releaseDirectory 'nwflash-desktop.exe')
    $installers = @(Get-ChildItem -LiteralPath (Join-Path $releaseDirectory 'bundle\nsis') -File -Filter '*-setup.exe')
    if ($installers.Count -ne 1) { throw 'Development NSIS installer is missing or ambiguous.' }
    Copy-Item -LiteralPath $exe -Destination (Join-Path $stage 'nwflash-desktop.exe')
    Copy-Item -LiteralPath $installers[0].FullName -Destination (Join-Path $stage $installers[0].Name)
    Copy-ApprovedResources -DestinationRoot (Join-Path $stage 'resources') -Entries $resourceEntries
    & (Join-Path $PSScriptRoot 'New-TauriReleaseManifest.ps1') -ReleaseRoot $stage -ResourceManifestPath $resourceManifestPath -DevelopmentUnsigned
    & (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $stage -ResourceManifestPath $resourceManifestPath
    Write-Host 'Development unsigned release staging completed.'
    return
}

$acceptedChain = Assert-AcceptedEvidenceChain -AcceptedEvidence $AcceptedEvidence -Operations (New-DefaultProtectionOperations)
$acceptedPath = [string]$acceptedChain.accepted_path
$accepted = $acceptedChain.accepted
if ([string]::IsNullOrWhiteSpace($env:NWFLASH_CERT_THUMBPRINT)) {
    throw 'NWFLASH_CERT_THUMBPRINT is required only when protected Finalize signing begins.'
}
$thumbprint = ($env:NWFLASH_CERT_THUMBPRINT -replace '\s', '').ToUpperInvariant()
if ($thumbprint -notmatch '^[0-9A-F]{40}$') { throw 'NWFLASH_CERT_THUMBPRINT must be a 40-character SHA-1 certificate thumbprint.' }

$packagingRoot = Join-Path $repo ('artifacts\vmp-packaging\' + [string]$accepted.handoff_id)
$packagingRoot = Initialize-FreshRoot -Root $packagingRoot -MarkerName '.nwflash-vmp-packaging' -MarkerValue ([string]$accepted.handoff_id)
$evidenceRoot = Join-Path $packagingRoot 'evidence'
$releaseDirectory = Join-Path $packagingRoot 'release'
New-Item -ItemType Directory -Path $evidenceRoot | Out-Null
New-Item -ItemType Directory -Path $releaseDirectory | Out-Null
$packagingExe = Join-Path $releaseDirectory 'nwflash-desktop.exe'
Copy-Item -LiteralPath ([string]$accepted.protected_output.path) -Destination $packagingExe
(Get-Item -LiteralPath $packagingExe).IsReadOnly = $false
if ((Get-Sha256Hex $packagingExe) -ne [string]$accepted.protected_output.sha256) { throw 'Packaging EXE copy does not match accepted protected output.' }
Copy-ApprovedResources -DestinationRoot (Join-Path $releaseDirectory 'resources') -Entries $resourceEntries

$exeEvidencePath = Join-Path $evidenceRoot 'exe-signed.json'
& (Join-Path $PSScriptRoot 'Sign-NwflashRelease.ps1') -Path $packagingExe `
    -ExpectedUnsignedSha256 ([string]$accepted.protected_output.sha256) -InputEvidence $acceptedPath `
    -SigningEvidenceOut $exeEvidencePath -State 'exe-signed'
$exeEvidence = Read-ProtectedEvidence -Path $exeEvidencePath -ExpectedState 'exe-signed'

$bundleDirectory = Join-Path $releaseDirectory 'bundle\nsis'
if (Test-Path -LiteralPath $bundleDirectory) { throw "Fresh packaging target unexpectedly contains NSIS output: $bundleDirectory" }
$priorTarget = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = $packagingRoot
    $bundleStarted = [DateTimeOffset]::UtcNow
    Invoke-CheckedExternalCommand -Description 'Fresh NSIS bundle build' -Command { npm --prefix $desktop run tauri -- bundle --no-sign }
}
finally {
    if ($null -eq $priorTarget) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue } else { $env:CARGO_TARGET_DIR = $priorTarget }
}
$installers = @(Get-ChildItem -LiteralPath $bundleDirectory -File -Filter '*-setup.exe')
if ($installers.Count -ne 1) { throw 'Fresh NSIS build did not produce exactly one installer.' }
if ($installers[0].LastWriteTimeUtc -le $bundleStarted.UtcDateTime) { throw 'NSIS installer is stale relative to the fresh bundle transition.' }
$installerUnsignedHash = Get-Sha256Hex $installers[0].FullName
$nsisEvidencePath = Join-Path $evidenceRoot 'nsis-built.json'
$nsisEvidence = [ordered]@{
    schema = 1
    handoff_id = [string]$accepted.handoff_id
    state = 'nsis-built'
    created_utc = [DateTimeOffset]::UtcNow.ToString('o')
    previous_evidence_sha256 = Get-Sha256Hex $exeEvidencePath
    signed_exe_sha256 = [string]$exeEvidence.signed_sha256
    installer_path = $installers[0].FullName
    installer_unsigned_sha256 = $installerUnsignedHash
    bundle_started_utc = $bundleStarted.ToString('o')
}
Write-AtomicEvidence -Path $nsisEvidencePath -Value $nsisEvidence | Out-Null

$installerEvidencePath = Join-Path $evidenceRoot 'installer-signed.json'
& (Join-Path $PSScriptRoot 'Sign-NwflashRelease.ps1') -Path $installers[0].FullName `
    -ExpectedUnsignedSha256 $installerUnsignedHash -InputEvidence $nsisEvidencePath `
    -SigningEvidenceOut $installerEvidencePath -State 'installer-signed'
$installedEvidencePath = Join-Path $evidenceRoot 'installed-verified.json'
& (Join-Path $PSScriptRoot 'Test-TauriInstaller.ps1') -InstallerPath $installers[0].FullName `
    -ExpectedExeSha256 ([string]$exeEvidence.signed_sha256) -ExpectedThumbprint $thumbprint `
    -UnprotectedSha256 ([string]$accepted.input_exe_sha256) -InstallerSignedEvidence $installerEvidencePath `
    -VerificationEvidenceOut $installedEvidencePath -ResourceManifestPath $resourceManifestPath

$stage = Initialize-FreshRoot -Root $ReleaseRoot -MarkerName '.nwflash-tauri-release' -MarkerValue ([string]$accepted.handoff_id)
Copy-Item -LiteralPath $packagingExe -Destination (Join-Path $stage 'nwflash-desktop.exe')
Copy-Item -LiteralPath $installers[0].FullName -Destination (Join-Path $stage $installers[0].Name)
Copy-ApprovedResources -DestinationRoot (Join-Path $stage 'resources') -Entries $resourceEntries
$releaseVerifiedPath = Join-Path $evidenceRoot 'release-verified.json'
$verifyArguments = @{
    ReleaseRoot = $stage
    AcceptedEvidence = $acceptedPath
    ExeSignedEvidence = $exeEvidencePath
    NsisBuiltEvidence = $nsisEvidencePath
    InstallerSignedEvidence = $installerEvidencePath
    InstalledVerifiedEvidence = $installedEvidencePath
    ExpectedThumbprint = $thumbprint
    ResourceManifestPath = $resourceManifestPath
}
& (Join-Path $PSScriptRoot 'Verify-ProtectedRelease.ps1') @verifyArguments -VerificationEvidenceOut $releaseVerifiedPath
& (Join-Path $PSScriptRoot 'New-TauriReleaseManifest.ps1') -ReleaseRoot $stage -ResourceManifestPath $resourceManifestPath -ReleaseVerifiedEvidence $releaseVerifiedPath
& (Join-Path $PSScriptRoot 'Verify-ProtectedRelease.ps1') @verifyArguments -RequireManifest

$manifested = [ordered]@{
    schema = 1
    handoff_id = [string]$accepted.handoff_id
    state = 'manifested'
    created_utc = [DateTimeOffset]::UtcNow.ToString('o')
    previous_evidence_sha256 = Get-Sha256Hex $releaseVerifiedPath
    manifest_sha256 = Get-Sha256Hex (Join-Path $stage 'SHA256SUMS.txt')
    signed_exe_sha256 = Get-Sha256Hex (Join-Path $stage 'nwflash-desktop.exe')
    signed_installer_sha256 = Get-Sha256Hex (Join-Path $stage $installers[0].Name)
}
$manifestedPath = Write-AtomicEvidence -Path (Join-Path $evidenceRoot 'manifested.json') -Value $manifested
Write-Output $manifestedPath
Write-Host 'Protected signed Tauri release provenance chain completed.'
