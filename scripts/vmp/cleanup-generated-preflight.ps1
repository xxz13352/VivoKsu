#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param([string]$Root)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).ProviderPath
$expected = [IO.Path]::GetFullPath((Join-Path $repo 'artifacts\vmp-preflight'))
if ([string]::IsNullOrWhiteSpace($Root)) { $Root = $expected }
$target = [IO.Path]::GetFullPath($Root)
if (-not $target.Equals($expected, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing cleanup outside the exact generated preflight root: $target"
}
if (-not (Test-Path -LiteralPath $target)) {
    Write-Host 'Generated preflight root is already absent.'
    return
}

. (Join-Path $PSScriptRoot 'protected-release-contract.ps1')
Assert-NoReparseAncestors $target
Get-ReparseSafeTreeEntries -Root $target | Out-Null
Remove-Item -LiteralPath $target -Recurse -Force
if (Test-Path -LiteralPath $target) {
    throw "Generated preflight cleanup did not remove the exact root: $target"
}
Write-Host 'Generated preflight root removed.'
