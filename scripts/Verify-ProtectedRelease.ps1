[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ReleaseRoot,
    [Parameter(Mandatory)][string]$AcceptedEvidence,
    [Parameter(Mandatory)][string]$ExeSignedEvidence,
    [Parameter(Mandatory)][string]$NsisBuiltEvidence,
    [Parameter(Mandatory)][string]$InstallerSignedEvidence,
    [Parameter(Mandatory)][string]$InstalledVerifiedEvidence,
    [string]$VerificationEvidenceOut,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{40}$')][string]$ExpectedThumbprint,
    [string]$ResourceManifestPath,
    [switch]$RequireManifest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')
if ([string]::IsNullOrWhiteSpace($ResourceManifestPath)) {
    $ResourceManifestPath = Join-Path $repo 'packaging\release\tauri-resources.json'
}
$root = Get-NormalizedFullPath $ReleaseRoot
if (-not (Test-Path -LiteralPath $root -PathType Container)) { throw "Release root is missing: $root" }

$acceptedPath = Resolve-FullyQualifiedLeaf $AcceptedEvidence
$exeEvidencePath = Resolve-FullyQualifiedLeaf $ExeSignedEvidence
$nsisEvidencePath = Resolve-FullyQualifiedLeaf $NsisBuiltEvidence
$installerEvidencePath = Resolve-FullyQualifiedLeaf $InstallerSignedEvidence
$installedEvidencePath = Resolve-FullyQualifiedLeaf $InstalledVerifiedEvidence
$accepted = Read-ProtectedEvidence -Path $acceptedPath -ExpectedState 'accepted'
$exeEvidence = Read-ProtectedEvidence -Path $exeEvidencePath -ExpectedState 'exe-signed'
$nsisEvidence = Read-ProtectedEvidence -Path $nsisEvidencePath -ExpectedState 'nsis-built'
$installerEvidence = Read-ProtectedEvidence -Path $installerEvidencePath -ExpectedState 'installer-signed'
$installedEvidence = Read-ProtectedEvidence -Path $installedEvidencePath -ExpectedState 'installed-verified'
$documents = @($accepted, $exeEvidence, $nsisEvidence, $installerEvidence, $installedEvidence)
if (@($documents | Where-Object { [string]$_.handoff_id -ne [string]$accepted.handoff_id }).Count -ne 0) {
    throw 'Protected evidence handoff IDs do not match.'
}

if ([string]$exeEvidence.previous_evidence_sha256 -ne (Get-Sha256Hex $acceptedPath) -or
    [string]$nsisEvidence.previous_evidence_sha256 -ne (Get-Sha256Hex $exeEvidencePath) -or
    [string]$installerEvidence.previous_evidence_sha256 -ne (Get-Sha256Hex $nsisEvidencePath) -or
    [string]$installedEvidence.previous_evidence_sha256 -ne (Get-Sha256Hex $installerEvidencePath)) {
    throw 'Protected evidence SHA-256 chain is broken.'
}
if ([string]$exeEvidence.unsigned_sha256 -ne [string]$accepted.protected_output.sha256) { throw 'EXE signing did not consume the accepted protected hash.' }
if ([string]$nsisEvidence.signed_exe_sha256 -ne [string]$exeEvidence.signed_sha256) { throw 'NSIS did not consume the signed protected EXE hash.' }
if ([string]$installerEvidence.unsigned_sha256 -ne [string]$nsisEvidence.installer_unsigned_sha256) { throw 'Installer signing did not consume the fresh NSIS hash.' }
if ([string]$installedEvidence.installed_exe_sha256 -ne [string]$exeEvidence.signed_sha256) { throw 'Installed EXE hash does not equal the signed protected EXE hash.' }

$exe = Resolve-FullyQualifiedLeaf (Join-Path $root 'nwflash-desktop.exe')
$installers = @(Get-ChildItem -LiteralPath $root -File -Filter '*-setup.exe')
if ($installers.Count -ne 1) { throw 'Protected NSIS installer is missing or ambiguous.' }
$installer = $installers[0].FullName
if ((Get-Sha256Hex $exe) -ne [string]$exeEvidence.signed_sha256) { throw 'Staged EXE hash does not match exe-signed evidence.' }
if ((Get-Sha256Hex $installer) -ne [string]$installerEvidence.signed_sha256) { throw 'Staged installer hash does not match installer-signed evidence.' }
if ((Get-Sha256Hex $exe) -eq [string]$accepted.input_exe_sha256) { throw 'Staged EXE equals the unprotected input hash.' }
foreach ($target in @($exe, $installer)) {
    Assert-AuthenticodeIdentity -Signature (Get-AuthenticodeSignature -LiteralPath $target) -ExpectedThumbprint $ExpectedThumbprint | Out-Null
}

$resourceManifest = Resolve-FullyQualifiedLeaf $ResourceManifestPath
$resources = @((Get-Content -Raw -LiteralPath $resourceManifest | ConvertFrom-Json).resources)
$allowed = @('nwflash-desktop.exe', $installers[0].Name, '.nwflash-tauri-release')
if ($RequireManifest) { $allowed += 'SHA256SUMS.txt' }
foreach ($entry in $resources) {
    $relative = 'resources/' + ([string]$entry.destination).Replace('\', '/')
    $resource = Resolve-FullyQualifiedLeaf (Join-Path $root $relative.Replace('/', '\'))
    if ((Get-Sha256Hex $resource) -ne ([string]$entry.sha256).ToUpperInvariant()) { throw "Staged resource hash mismatch: $relative" }
    $allowed += $relative
}
Assert-ExactFileSet -Root $root -AllowedRelativePaths $allowed
foreach ($file in Get-ChildItem -LiteralPath $root -Recurse -File -Force) {
    $relative = Get-RelativeFilePath -Root $root -Path $file.FullName
    if ($relative -match '(?i)(\.pdb$|\.map$|\.lib$|\.exp$|\.ilk$|compiler.*\.log$|marker-review\.json$|prepared\.json$|accepted\.json$|VMProtect)' -or
        $relative -match '(?i)(^|/)(sdk|include|lib)(/|$)') {
        throw "Forbidden release artifact: $relative"
    }
    if ((Get-Sha256Hex $file.FullName) -eq [string]$accepted.input_exe_sha256) { throw "Release contains the unprotected EXE bytes: $relative" }
}

if ($RequireManifest) {
    & (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $root -ResourceManifestPath $resourceManifest
    if ($LASTEXITCODE -ne 0) { throw 'Final SHA-256 manifest verification failed.' }
}
if (-not [string]::IsNullOrWhiteSpace($VerificationEvidenceOut)) {
    $verification = [ordered]@{
        schema = 1
        handoff_id = [string]$accepted.handoff_id
        state = 'release-verified'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = Get-Sha256Hex $installedEvidencePath
        signed_exe_sha256 = Get-Sha256Hex $exe
        signed_installer_sha256 = Get-Sha256Hex $installer
        unprotected_exe_sha256 = [string]$accepted.input_exe_sha256
        exact_release_tree = $true
        manifest_verified = [bool]$RequireManifest
    }
    Write-AtomicEvidence -Path $VerificationEvidenceOut -Value $verification | Out-Null
}
Write-Host 'Protected release provenance verification passed.'
