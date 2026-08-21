[CmdletBinding()]
param([string]$ReleaseRoot)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ReleaseRoot)) { $ReleaseRoot = Join-Path $repo 'artifacts\tauri-release' }
$root = [IO.Path]::GetFullPath($ReleaseRoot)
$exe = Join-Path $root 'nwflash-desktop.exe'
if (-not (Test-Path $exe)) { throw "Release executable is missing: $exe" }
if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_PATH)) { throw 'NWFLASH_VMP_PATH is required for VMProtect.' }
if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_PROJECT)) { throw 'NWFLASH_VMP_PROJECT is required for VMProtect.' }
if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_ARGUMENTS)) { throw 'NWFLASH_VMP_ARGUMENTS must be a JSON argument array containing {project}, {input}, and {output} placeholders.' }
if (-not (Test-Path $env:NWFLASH_VMP_PATH)) { throw "VMProtect executable is missing: $env:NWFLASH_VMP_PATH" }

$project = [IO.Path]::GetFullPath($env:NWFLASH_VMP_PROJECT)
if (-not (Test-Path -LiteralPath $project -PathType Leaf)) { throw "VMProtect project is missing: $project" }
$repoPrefix = $repo.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if ($project.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'NWFLASH_VMP_PROJECT must reference an externally controlled VMProtect project.'
}
$output = Join-Path $root '.nwflash-desktop.protected.exe'
if ([IO.Path]::GetFullPath($output).Equals([IO.Path]::GetFullPath($exe), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'VMProtect output must not overwrite the unprotected input directly.'
}
$arguments = ConvertFrom-Json $env:NWFLASH_VMP_ARGUMENTS
if ($arguments -isnot [Array]) { throw 'NWFLASH_VMP_ARGUMENTS must be a JSON array.' }
$argumentTexts = @($arguments | ForEach-Object {
    if ($_ -isnot [string]) { throw 'NWFLASH_VMP_ARGUMENTS must contain only string arguments.' }
    $_
})
foreach ($placeholder in @('{project}', '{input}', '{output}')) {
    if (-not ($argumentTexts | Where-Object { $_.Contains($placeholder) })) {
        throw "NWFLASH_VMP_ARGUMENTS must contain the $placeholder placeholder."
    }
}
if (Test-Path -LiteralPath $output) { Remove-Item -LiteralPath $output -Force }
$inputHash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
$resolvedArguments = @($argumentTexts | ForEach-Object {
    $_.Replace('{project}', $project).Replace('{input}', $exe).Replace('{output}', $output)
})
& $env:NWFLASH_VMP_PATH @resolvedArguments
if ($LASTEXITCODE -ne 0) { throw "VMProtect failed with exit code $LASTEXITCODE." }
if (-not (Test-Path -LiteralPath $output -PathType Leaf) -or (Get-Item -LiteralPath $output).Length -le 0) {
    throw 'VMProtect did not produce protected output.'
}
$outputHash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash
if ($outputHash.Equals($inputHash, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'VMProtect did not produce protected output distinct from the input.'
}
Copy-Item -LiteralPath $output -Destination $exe -Force
Remove-Item -LiteralPath $output -Force
