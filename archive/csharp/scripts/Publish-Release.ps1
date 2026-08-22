[CmdletBinding()]
param(
    [string]$Configuration = "Release",
    [string]$RuntimeIdentifier = "win-x64"
)

# NOTE: keep this file ASCII-only. On this machine a BOM-less UTF-8 .ps1 is read as
# GBK, and non-ASCII comments corrupt the parser. English comments only.

$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$projectPath = Join-Path $repositoryRoot "src\VivoKsu.App\VivoKsu.App.csproj"
$bootstrapperProjectPath = Join-Path $repositoryRoot "src\VivoKsu.Bootstrapper\VivoKsu.Bootstrapper.csproj"
$outputPath = Join-Path $repositoryRoot "artifacts\release\VivoKsu-$RuntimeIdentifier"
$releaseDirectory = Split-Path -Parent $outputPath
$archivePath = Join-Path $releaseDirectory "VivoKsu-$RuntimeIdentifier.zip"

Write-Host "Restoring $RuntimeIdentifier assets..."
dotnet restore $projectPath -r $RuntimeIdentifier

# ---- framework-dependent publish (no bundled .NET runtime) ----
# The runtime is detected on first launch by the native AOT launcher
# (VivoKsu.Launcher.exe): if missing it downloads the Microsoft installer
# and silently installs it.
Write-Host "Publishing framework-dependent application..."
dotnet publish $projectPath `
    -c $Configuration `
    -r $RuntimeIdentifier `
    --self-contained false `
    --no-restore `
    -o $outputPath

# ---- native AOT launcher ----
# AOT linking needs vswhere.exe to locate the VC++ toolchain (MSVC BuildTools is
# installed); prepend its folder to PATH so the ILCompiler can find it.
$vsInstaller = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer"
if (Test-Path -LiteralPath $vsInstaller) {
    $env:PATH = "$vsInstaller;$env:PATH"
}
$bootstrapperStaging = Join-Path $outputPath "bootstrapper-staging"
Write-Host "Publishing native AOT bootstrapper..."
dotnet publish $bootstrapperProjectPath `
    -c $Configuration `
    -r $RuntimeIdentifier `
    -o $bootstrapperStaging

$launcherSource = Join-Path $bootstrapperStaging "VivoKsu.Bootstrapper.exe"
if (-not (Test-Path -LiteralPath $launcherSource)) {
    throw "AOT launcher publish failed: VivoKsu.Bootstrapper.exe not found."
}
Copy-Item -LiteralPath $launcherSource -Destination (Join-Path $outputPath "VivoKsu.Launcher.exe") -Force
Remove-Item -LiteralPath $bootstrapperStaging -Recurse -Force

$manifestPath = Join-Path $outputPath "SHA256SUMS.txt"

# ---- Release size reduction ----
# Trim satellite language packs: keep only Chinese/English.
$keepLanguages = @("zh-Hans", "zh-Hant")
Get-ChildItem $outputPath -Directory |
    Where-Object { $_.Name -match '^[a-z]{2}(-[A-Za-z]+)?$' -and $_.Name -notin $keepLanguages } |
    Remove-Item -Recurse -Force

Get-ChildItem $outputPath -File -Recurse |
    Where-Object { $_.FullName -ne $manifestPath } |
    Sort-Object FullName |
    ForEach-Object {
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash
        $relativePath = $_.FullName.Substring($outputPath.Length).TrimStart('\')
        "$hash  $relativePath"
    } |
    Set-Content -Path $manifestPath -Encoding ascii

Write-Host "Published to $outputPath"
Write-Host "Manifest: $manifestPath"

Write-Host "Creating release archive..."
Compress-Archive -Path (Join-Path $outputPath "*") -DestinationPath $archivePath -Force
$archiveHash = (Get-FileHash $archivePath -Algorithm SHA256).Hash
Set-Content -Path "$archivePath.sha256" -Value "$archiveHash  $(Split-Path -Leaf $archivePath)" -Encoding ascii
Write-Host "Archive: $archivePath"
