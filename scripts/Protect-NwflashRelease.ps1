#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Console')][string]$Mode,
    [Parameter(Mandatory)][string]$PreparedManifest,
    [Parameter(Mandatory)][string]$ConsolePath,
    [Parameter(Mandatory)][string]$ProjectPath,
    [Parameter(Mandatory)][string[]]$ConsoleArguments,
    [Parameter(Mandatory)][string]$MarkerReviewPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
throw 'Automated VMProtect console execution is disabled. Use Publish-TauriRelease.ps1 -PrepareManual, process the immutable handoff with VMProtect Lite GUI, then run accept-manual-output.ps1.'
