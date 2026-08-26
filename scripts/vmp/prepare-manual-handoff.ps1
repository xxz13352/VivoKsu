[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InputExe,
    [Parameter(Mandatory)][string]$InputPdb,
    [Parameter(Mandatory)][string]$InputMap,
    [Parameter(Mandatory)][string]$ProtectedOutputPath,
    [Parameter(Mandatory)][string]$CompilerLogPath,
    [Parameter(Mandatory)][string]$HandoffRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'protected-release-contract.ps1')

if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
    throw 'Protected handoff preparation requires Windows x64.'
}

$operations = New-DefaultProtectionOperations
$manifest = Invoke-PrepareManualHandoffCore -InputExe $InputExe -InputPdb $InputPdb -InputMap $InputMap `
    -ProtectedOutputPath $ProtectedOutputPath -CompilerLogPath $CompilerLogPath `
    -HandoffRoot $HandoffRoot -Operations $operations
Write-Output $manifest
Write-Output 'HANDOFF_REQUIRED'
