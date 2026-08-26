[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PreparedManifest,
    [Parameter(Mandatory)][string]$MarkerReviewPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'protected-release-contract.ps1')

if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
    throw 'Protected handoff acceptance requires Windows x64.'
}

$operations = New-DefaultProtectionOperations
$accepted = Invoke-AcceptManualOutputCore -PreparedManifest $PreparedManifest `
    -MarkerReviewPath $MarkerReviewPath -Operations $operations
Write-Output $accepted
Write-Output 'ACCEPTED'
