[CmdletBinding()]
param([switch]$DryRun, [string]$ReleaseRoot)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ReleaseRoot)) { $ReleaseRoot = Join-Path $repo 'artifacts\tauri-release' }
$root = [IO.Path]::GetFullPath($ReleaseRoot)
if ($DryRun) { Write-Host "Protected-release dry run root: $root"; exit 0 }
if ([string]::IsNullOrWhiteSpace($env:NWFLASH_CERT_THUMBPRINT)) { throw 'NWFLASH_CERT_THUMBPRINT is required for protected-release verification.' }
$expectedThumbprint = ($env:NWFLASH_CERT_THUMBPRINT -replace '\s', '').ToUpperInvariant()
if ($expectedThumbprint -notmatch '^[0-9A-F]{40}$') { throw 'NWFLASH_CERT_THUMBPRINT must be a 40-character SHA-1 certificate thumbprint.' }

& (Join-Path $PSScriptRoot 'Verify-TauriRelease.ps1') -ReleaseRoot $root
$exe = Join-Path $root 'nwflash-desktop.exe'
$installer = @(Get-ChildItem -LiteralPath $root -File -Filter '*-setup.exe')
if ($installer.Count -ne 1) { throw 'Protected NSIS installer is missing or ambiguous.' }
foreach ($target in @($exe, $installer[0].FullName)) {
    $signature = Get-AuthenticodeSignature $target
    if ($signature.Status -ne 'Valid') { throw "Authenticode signature is not valid for ${target}: $($signature.Status)" }
    $actualThumbprint = ($signature.SignerCertificate.Thumbprint -replace '\s', '').ToUpperInvariant()
    if ($actualThumbprint -ne $expectedThumbprint) {
        throw "Authenticode signature certificate does not match NWFLASH_CERT_THUMBPRINT for $target."
    }
}
Write-Host 'Protected release verification passed.'
