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
