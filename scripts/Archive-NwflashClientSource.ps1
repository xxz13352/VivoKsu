$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$sourceRoot = (Resolve-Path (Join-Path $repoRoot 'src\Nwflash.Desktop')).Path
$artifactDirectory = Join-Path $repoRoot 'artifacts\source'
$archivePath = Join-Path $artifactDirectory 'Nwflash.Desktop-source-20260818.zip'
$hashPath = "$archivePath.sha256"
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$stageRoot = Join-Path $tempRoot ("nwflash-client-source-archive-" + [Guid]::NewGuid().ToString('N'))
$stageClient = Join-Path $stageRoot 'Nwflash.Desktop'
$compileExit = 0

function Get-SourceRelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Path
    )

    $rootUri = [Uri](($Root.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar))
    return [Uri]::UnescapeDataString($rootUri.MakeRelativeUri([Uri]$Path).ToString()).Replace('/', '\')
}

try {
    if (Test-Path -LiteralPath $archivePath) {
        throw "Refusing to overwrite an existing archive: $archivePath"
    }

    [IO.Directory]::CreateDirectory($stageClient) | Out-Null
    [IO.Directory]::CreateDirectory($artifactDirectory) | Out-Null

    $excludedDirectoryNames = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    @(
        '.git', '.vite', 'node_modules', 'dist', 'target', 'logs',
        'tauri-release', 'vmprotect', 'gen', 'coverage', 'test-results',
        'playwright-report'
    ) | ForEach-Object { [void]$excludedDirectoryNames.Add($_) }

    $directories = [Collections.Generic.Stack[string]]::new()
    $directories.Push($sourceRoot)
    $fileCount = 0
    $sourceBytes = [int64]0

    while ($directories.Count -gt 0) {
        $current = $directories.Pop()
        foreach ($entry in (Get-ChildItem -LiteralPath $current -Force)) {
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                continue
            }

            if ($entry.PSIsContainer) {
                if ($excludedDirectoryNames.Contains($entry.Name)) {
                    continue
                }

                $relativeDirectory = Get-SourceRelativePath -Root $sourceRoot -Path $entry.FullName
                [IO.Directory]::CreateDirectory((Join-Path $stageClient $relativeDirectory)) | Out-Null
                $directories.Push($entry.FullName)
                continue
            }

            $relativeFile = Get-SourceRelativePath -Root $sourceRoot -Path $entry.FullName
            $destination = Join-Path $stageClient $relativeFile
            [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
            Copy-Item -LiteralPath $entry.FullName -Destination $destination
            $fileCount++
            $sourceBytes += [int64]$entry.Length
        }
    }

    Compress-Archive -LiteralPath $stageClient -DestinationPath $archivePath -CompressionLevel Optimal
    $archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToUpperInvariant()
    Set-Content -LiteralPath $hashPath -Value ("$archiveHash  " + (Split-Path -Leaf $archivePath)) -Encoding ascii

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $entryNames = @($zip.Entries | ForEach-Object { $_.FullName })
        $requiredEntries = @(
            'Nwflash.Desktop/README.md',
            'Nwflash.Desktop/docs/rust-tauri-architecture.md',
            'Nwflash.Desktop/src-tauri/Cargo.toml',
            'Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs',
            'Nwflash.Desktop/src/app/App.tsx'
        )
        $missing = @($requiredEntries | Where-Object { $_ -notin $entryNames })
        $forbidden = @($entryNames | Where-Object {
            $_ -match '/(?:\.git|\.vite|node_modules|dist|target|logs|tauri-release|vmprotect|gen|coverage|test-results|playwright-report)(?:/|$)'
        })
        if ($missing.Count -gt 0) {
            throw "Archive is missing required entries: $($missing -join ', ')"
        }
        if ($forbidden.Count -gt 0) {
            throw "Archive contains excluded entries: $($forbidden -join ', ')"
        }
        Write-Output "ARCHIVE_ENTRIES=$($entryNames.Count)"
    }
    finally {
        $zip.Dispose()
    }

    Write-Output "ARCHIVE_FILES=$fileCount"
    Write-Output "ARCHIVE_SOURCE_BYTES=$sourceBytes"
    Write-Output "ARCHIVE_PATH=$archivePath"
    Write-Output "ARCHIVE_SHA256=$archiveHash"

    Push-Location $repoRoot
    try {
        Write-Output 'COMPILATION_START'
        & npm --prefix src/Nwflash.Desktop run tauri -- build --no-bundle
        $compileExit = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }

    if ($compileExit -ne 0) {
        throw "Tauri no-bundle build exited with code $compileExit"
    }

    $executable = Join-Path $sourceRoot 'src-tauri\target\release\nwflash-desktop.exe'
    if (-not (Test-Path -LiteralPath $executable)) {
        throw "Compilation reported success but executable was not found: $executable"
    }
    Write-Output "COMPILED_EXECUTABLE=$executable"
}
catch {
    Write-Error $_
    if ($compileExit -eq 0) {
        $compileExit = 1
    }
}
finally {
    $resolvedStage = [IO.Path]::GetFullPath($stageRoot)
    $safeToDelete = $resolvedStage.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
        $resolvedStage.Contains('nwflash-client-source-archive-')
    if ($safeToDelete -and (Test-Path -LiteralPath $stageRoot)) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force
        Write-Output "STAGING_REMOVED=$stageRoot"
    }
}

if ($compileExit -ne 0) {
    exit $compileExit
}

$sidecarHash = ((Get-Content -LiteralPath $hashPath -Raw).Trim() -split '\s+')[0].ToUpperInvariant()
$finalHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToUpperInvariant()
if ($sidecarHash -ne $finalHash) {
    throw "Archive hash sidecar mismatch: $sidecarHash != $finalHash"
}

Write-Output "DELIVERY_ARCHIVE_EXISTS=$(Test-Path -LiteralPath $archivePath)"
Write-Output "DELIVERY_HASH_EXISTS=$(Test-Path -LiteralPath $hashPath)"
Write-Output "STAGING_EXISTS=$(Test-Path -LiteralPath $stageRoot)"
Write-Output "FINAL_SHA256=$finalHash"
