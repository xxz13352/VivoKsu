[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repo 'src\Nwflash.Desktop\src-tauri\Cargo.toml'
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')

$metadata = (& cargo metadata --manifest-path $manifest --format-version 1 --no-deps 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata probe failed.' }
$rootPackage = @((ConvertFrom-Json $metadata).packages | Where-Object name -eq 'nwflash-desktop')
if ($rootPackage.Count -ne 1) { throw 'Cargo metadata did not identify the desktop package.' }
foreach ($name in @('tauri-plugin-wdio', 'tauri-plugin-wdio-webdriver')) {
    $dependency = @($rootPackage[0].dependencies | Where-Object name -eq $name)
    if ($dependency.Count -ne 1 -or -not [bool]$dependency[0].optional) { throw "$name must be an optional root dependency." }
}

$protectedTree = (& cargo tree --manifest-path $manifest -p nwflash-desktop --no-default-features --features protected --edges features -i nwflash-protection 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { throw 'Protected Cargo feature graph probe failed.' }
if ($protectedTree -notmatch 'nwflash-protection feature "vmp-sdk"' -or $protectedTree -notmatch 'nwflash-tauri feature "protected"') {
    throw 'Protected feature does not propagate to nwflash-protection/vmp-sdk.'
}
if ($protectedTree -match 'tauri-plugin-wdio') { throw 'Protected Cargo graph activates a WDIO plugin.' }

$targetRoot = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-profile-probe-' + [Guid]::NewGuid().ToString('N'))
try {
    $verbose = (& cargo build --manifest-path $manifest -p nwflash-protection --release --target-dir $targetRoot -vv 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Protected release-profile compiler probe failed.' }
    $command = @($verbose -split "`r?`n" | Where-Object { $_ -match 'rustc.*--crate-name nwflash_protection' } | Select-Object -Last 1)
    if ($command.Count -ne 1) { throw 'Cargo verbose output omitted the nwflash-protection rustc invocation.' }
    foreach ($flag in @('-C opt-level=3', '-C panic=abort', '-C codegen-units=1', '-C debuginfo=2', '-C split-debuginfo=packed')) {
        if (-not $command[0].Contains($flag)) { throw "Effective release profile omitted $flag." }
    }
    if (-not ($command[0].Contains('-C linker-plugin-lto') -or $command[0].Contains('-C lto=fat'))) { throw 'Effective release profile omitted fat LTO.' }
    if ($command[0] -match '-C (?:incremental|strip=(?:symbols|debuginfo))') { throw 'Effective release profile enabled incremental compilation or stripping.' }
    Write-Host 'Effective protected Cargo profile and feature graph passed.'
}
finally {
    if (Test-Path -LiteralPath $targetRoot) { Remove-ValidatedTemporaryRoot -Root $targetRoot -Prefix 'nwflash-profile-probe-' }
}
