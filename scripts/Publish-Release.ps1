[CmdletBinding()]
param(
    [string]$Configuration = "Release",
    [string]$RuntimeIdentifier = "win-x64"
)

$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$projectPath = Join-Path $repositoryRoot "src\VivoKsu.App\VivoKsu.App.csproj"
$outputPath = Join-Path $repositoryRoot "artifacts\release\VivoKsu-$RuntimeIdentifier"
$releaseDirectory = Split-Path -Parent $outputPath
$archivePath = Join-Path $releaseDirectory "VivoKsu-$RuntimeIdentifier.zip"

Write-Host "Restoring $RuntimeIdentifier assets..."
dotnet restore $projectPath -r $RuntimeIdentifier

Write-Host "Publishing self-contained application..."
dotnet publish $projectPath `
    -c $Configuration `
    -r $RuntimeIdentifier `
    --self-contained true `
    --no-restore `
    -o $outputPath

$bundledScrcpyExecutable = Join-Path $outputPath "scrcpy\scrcpy.exe"
$bundledScrcpyServer = Join-Path $outputPath "scrcpy\scrcpy-server"
if (-not (Test-Path -LiteralPath $bundledScrcpyExecutable) -or -not (Test-Path -LiteralPath $bundledScrcpyServer)) {
    throw "发布目录缺少内置 scrcpy 资源，已停止打包。"
}

$manifestPath = Join-Path $outputPath "SHA256SUMS.txt"

# ---- Release size reduction ----
# 1. Remove WinForms assemblies (pure WPF app does not need them)
$winFormsAssemblies = @(
    "System.Windows.Forms.dll",
    "System.Windows.Forms.Design.dll",
    "System.Windows.Forms.Design.Editors.dll",
    "System.Windows.Forms.Primitives.dll",
    "Accessibility.dll"
)
foreach ($assembly in $winFormsAssemblies) {
    $winFormsPath = Join-Path $outputPath $assembly
    if (Test-Path -LiteralPath $winFormsPath) {
        Remove-Item -LiteralPath $winFormsPath -Force
    }
}

# 2. Trim satellite language packs: keep only Chinese/English
$keepLanguages = @("zh-Hans", "zh-Hant")
Get-ChildItem $outputPath -Directory |
    Where-Object { $_.Name -match '^[a-z]{2}(-[A-Za-z]+)?$' -and $_.Name -notin $keepLanguages } |
    Remove-Item -Recurse -Force

# 3. Remove scrcpy-bundled adb (app sets the ADB env var to platform-tools\adb.exe)
$scrcpyRedundant = @(
    (Join-Path $outputPath "scrcpy\adb.exe"),
    (Join-Path $outputPath "scrcpy\AdbWinApi.dll"),
    (Join-Path $outputPath "scrcpy\AdbWinUsbApi.dll")
)
foreach ($asset in $scrcpyRedundant) {
    if (Test-Path -LiteralPath $asset) {
        Remove-Item -LiteralPath $asset -Force
    }
}

$obsoleteReleaseAssets = @(
    (Join-Path $outputPath "apk\Sukisu.APK"),
    (Join-Path $outputPath "platform-tools\ksud.exe"),
    (Join-Path $outputPath "platform-tools\libksud.exe")
)
foreach ($asset in $obsoleteReleaseAssets) {
    if (Test-Path -LiteralPath $asset) {
        Remove-Item -LiteralPath $asset -Force
    }
}
Get-ChildItem $outputPath -File -Recurse |
    Where-Object { $_.FullName -ne $manifestPath -and $_.Name -ne "Sukisu.APK" -and $_.Name -ne "ksud.exe" -and $_.Name -ne "libksud.exe" } |
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
