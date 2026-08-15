[CmdletBinding()]
param(
    [string]$Owner = "xxz13352",
    [string]$Repository = "NWFlash",
    [string]$ReleaseTag = "v1.0.0"
)

# Uploads the externalized release assets to a GitHub PUBLIC repository:
#   KSU.APK, KernelSU.apk, payload_dumper-win-x64.zip
# Target: xxz13352/NWFlash (see RemoteAssetCatalog for the constants the client reads).
# Prereq: gh CLI (https://cli.github.com) installed and authenticated (gh auth login).
# Creates the release if it does not exist, then uploads each asset. Idempotent re-runs
# overwrite existing assets (--clobber). Prints SHA256 for each asset at the end.
#
# NOTE: keep this file ASCII-only. On this machine a BOM-less UTF-8 .ps1 is read as GBK,
# and non-ASCII comments corrupt the parser. English comments only.
#
# Manual browser alternative (no gh):
#   1. On github.com/xxz13352/NWFlash create a release tagged v1.0.0.
#   2. Drag-drop the three files below into the release's Assets box.
#   3. Compute SHA256 (certutil -hashfile <file> SHA256) and paste into code.
#
# After upload, sync code constants:
#   - apk/KSU.APK        -> VivoRootResourceService.ManagerApkSha256 ["KSU"]
#   - apk/KernelSU.apk   -> VivoRootResourceService.ManagerApkSha256 ["OfficialKsu"]
#   - payload_dumper.zip -> RemoteAssetCatalog (the CLIENT verifies the extracted
#                           payload_dumper.exe against PayloadDumperSha256, so the
#                           zip itself only needs to contain the real exe).

$ErrorActionPreference = "Stop"

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw "gh CLI is required. Install from https://cli.github.com and run 'gh auth login' first."
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$appSource = Join-Path $repositoryRoot "src\VivoKsu.App"
$apkKsu = Join-Path $appSource "apk\KSU.APK"
$apkKernelSu = Join-Path $appSource "apk\KernelSU.apk"
$payloadExe = Join-Path $appSource "payload-tools\payload_dumper.exe"
$payloadZip = Join-Path $repositoryRoot "artifacts\resources\payload_dumper-win-x64.zip"

foreach ($file in @($apkKsu, $apkKernelSu, $payloadExe)) {
    if (-not (Test-Path -LiteralPath $file)) {
        throw "Missing source asset: $file"
    }
}

Write-Host "Packaging payload_dumper-win-x64.zip..."
$payloadZipDir = Split-Path -Parent $payloadZip
if (-not (Test-Path -LiteralPath $payloadZipDir)) {
    New-Item -ItemType Directory -Path $payloadZipDir -Force | Out-Null
}
# payload_dumper.exe must sit at the zip ROOT so the client extracts it as payload_dumper.exe.
$tempZipDir = Join-Path $payloadZipDir ".staging"
if (Test-Path -LiteralPath $tempZipDir) { Remove-Item -LiteralPath $tempZipDir -Recurse -Force }
New-Item -ItemType Directory -Path $tempZipDir -Force | Out-Null
Copy-Item -LiteralPath $payloadExe -Destination $tempZipDir
if (Test-Path -LiteralPath $payloadZip) { Remove-Item -LiteralPath $payloadZip -Force }
Compress-Archive -Path (Join-Path $tempZipDir "*") -DestinationPath $payloadZip -Force
Remove-Item -LiteralPath $tempZipDir -Recurse -Force

Write-Host "Checking release $ReleaseTag on $Owner/$Repository..."
if (-not (gh release view $ReleaseTag --repo "$Owner/$Repository" --json tagName --jq .tagName 2>$null)) {
    Write-Host "Creating release $ReleaseTag..."
    gh release create $ReleaseTag --repo "$Owner/$Repository" --title $ReleaseTag --notes "Externalized runtime assets for Nwflash (ROOT manager APKs + payload_dumper)." --latest
}

Write-Host "Uploading assets..."
gh release upload $ReleaseTag --repo "$Owner/$Repository" --clobber `
    "$apkKsu" `
    "$apkKernelSu" `
    "$payloadZip"

Write-Host ""
Write-Host "=== SHA256SUMS (keep in sync with code) ==="
foreach ($file in @($apkKsu, $apkKernelSu, $payloadZip)) {
    $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
    $name = Split-Path -Leaf $file
    Write-Host "$hash  $name"
}
Write-Host ""
Write-Host "KSU.APK / KernelSU.apk -> VivoRootResourceService.ManagerApkSha256."
Write-Host "The client verifies the EXTRACTED payload_dumper.exe against RemoteAssetCatalog.PayloadDumperSha256 (the zip is only transport)."
