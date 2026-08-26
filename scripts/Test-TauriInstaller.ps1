[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InstallerPath,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{64}$')][string]$ExpectedExeSha256,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{40}$')][string]$ExpectedThumbprint,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{64}$')][string]$UnprotectedSha256,
    [Parameter(Mandatory)][string]$InstallerSignedEvidence,
    [Parameter(Mandatory)][string]$VerificationEvidenceOut,
    [string]$InstallRoot,
    [string]$ResourceManifestPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')
if ([string]::IsNullOrWhiteSpace($ResourceManifestPath)) {
    $ResourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
}
$installer = Resolve-FullyQualifiedLeaf $InstallerPath
$manifest = Resolve-FullyQualifiedLeaf $ResourceManifestPath
$installerEvidencePath = Resolve-FullyQualifiedLeaf $InstallerSignedEvidence
$installerEvidence = Read-ProtectedEvidence -Path $installerEvidencePath -ExpectedState 'installer-signed'
if ((Get-Sha256Hex $installer) -ne [string]$installerEvidence.signed_sha256) {
    throw 'Installer hash does not match installer-signed evidence.'
}
$generatedRoot = [string]::IsNullOrWhiteSpace($InstallRoot)
if ($generatedRoot) {
    $InstallRoot = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-tauri-installer-' + [Guid]::NewGuid().ToString('N'))
}
$installRootPath = Get-NormalizedFullPath $InstallRoot
if (Test-Path -LiteralPath $installRootPath) {
    if (Get-ChildItem -LiteralPath $installRootPath -Force | Select-Object -First 1) { throw "Install root must be fresh and empty: $installRootPath" }
}

try {
    $installation = Start-Process -FilePath $installer -ArgumentList @('/S', "/D=$installRootPath") -WindowStyle Hidden -Wait -PassThru
    if ($installation.ExitCode -ne 0) { throw "NSIS installation failed with exit code $($installation.ExitCode)." }

    $resourceHashes = @{}
    $resources = @((Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).resources)
    foreach ($entry in $resources) {
        $relative = 'resources/' + ([string]$entry.destination).Replace('\', '/')
        $resourceHashes[$relative] = ([string]$entry.sha256).ToUpperInvariant()
    }
    $installedExe = Join-Path $installRootPath 'nwflash-desktop.exe'
    $signature = Get-AuthenticodeSignature -LiteralPath $installedExe
    $signatureIdentity = Assert-AuthenticodeIdentity -Signature $signature -ExpectedThumbprint $ExpectedThumbprint
    Assert-InstalledTreeContract -InstallRoot $installRootPath -ExpectedExeSha256 $ExpectedExeSha256 `
        -ResourceHashes $resourceHashes -Signature $signature -ExpectedThumbprint $ExpectedThumbprint `
        -UnprotectedSha256 $UnprotectedSha256

    $uninstaller = Resolve-FullyQualifiedLeaf (Join-Path $installRootPath 'uninstall.exe')
    $uninstallation = Start-Process -FilePath $uninstaller -ArgumentList '/S' -WindowStyle Hidden -Wait -PassThru
    if ($uninstallation.ExitCode -ne 0) { throw "NSIS uninstallation failed with exit code $($uninstallation.ExitCode)." }
    if (Test-Path -LiteralPath $installRootPath) { throw 'NSIS uninstaller left the installation directory behind.' }

    $evidence = [ordered]@{
        schema = 1
        handoff_id = [string]$installerEvidence.handoff_id
        state = 'installed-verified'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = Get-Sha256Hex $installerEvidencePath
        installer_signed_sha256 = Get-Sha256Hex $installer
        installed_exe_sha256 = $ExpectedExeSha256.ToUpperInvariant()
        signed_exe_sha256 = $ExpectedExeSha256.ToUpperInvariant()
        unprotected_exe_sha256 = $UnprotectedSha256.ToUpperInvariant()
        certificate = $signatureIdentity
        installed_tree_exact = $true
        uninstall_verified = $true
    }
    Write-AtomicEvidence -Path $VerificationEvidenceOut -Value $evidence | Out-Null
}
finally {
    if ($generatedRoot -and (Test-Path -LiteralPath $installRootPath)) {
        Remove-ValidatedTemporaryRoot -Root $installRootPath -Prefix 'nwflash-tauri-installer-'
    }
}

Write-Host 'Tauri installer exact-content verification passed.'
