[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$DestinationRoot
)

$ErrorActionPreference = "Stop"
$apiUri = "https://api.github.com/repos/Genymobile/scrcpy/releases/latest"
$destination = [IO.Path]::GetFullPath($DestinationRoot)
$requiredFiles = @("scrcpy.exe", "scrcpy-server")

function Test-ScrcpyPayload([string]$root) {
    foreach ($requiredFile in $requiredFiles) {
        $match = Get-ChildItem -LiteralPath $root -Recurse -File -Filter $requiredFile -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($null -eq $match -or $match.Length -le 0) {
            return $false
        }
    }

    return $true
}

if (Test-ScrcpyPayload $destination) {
    Write-Host "Bundled scrcpy is already present: $destination"
    exit 0
}

$headers = @{ "User-Agent" = "VivoKsu-Build/1.0" }
$release = Invoke-RestMethod -Uri $apiUri -Headers $headers
$asset = $release.assets |
    Where-Object { $_.name -like "scrcpy-win64-v*.zip" } |
    Select-Object -First 1

if ($null -eq $asset) {
    throw "GitHub latest scrcpy release has no Windows x64 ZIP asset."
}

$staging = Join-Path ([IO.Path]::GetTempPath()) ("VivoKsu-scrcpy-" + [Guid]::NewGuid().ToString("N"))
$archivePath = Join-Path $staging $asset.name
$extractPath = Join-Path $staging "extract"

try {
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    Invoke-WebRequest -Uri $asset.browser_download_url -Headers $headers -OutFile $archivePath
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractPath -Force
    $payloadRoot = Get-ChildItem -LiteralPath $extractPath -Directory | Select-Object -First 1
    if ($null -eq $payloadRoot) {
        throw "The scrcpy ZIP archive is empty."
    }

    if (Test-Path -LiteralPath $destination) {
        Remove-Item -LiteralPath $destination -Recurse -Force
    }
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    Copy-Item -Path (Join-Path $payloadRoot.FullName "*") -Destination $destination -Recurse -Force

    if (-not (Test-ScrcpyPayload $destination)) {
        throw "Bundled scrcpy is incomplete: scrcpy.exe or scrcpy-server is missing."
    }

    Write-Host "Bundled scrcpy $($release.tag_name) into $destination"
}
finally {
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
    }
}
