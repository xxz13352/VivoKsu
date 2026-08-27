#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SdkRoot,
    [switch]$AsJson
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not [System.IO.Path]::IsPathFullyQualified($SdkRoot)) {
    throw 'Test input SdkRoot must be fully qualified.'
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).ProviderPath
$manifestPath = Join-Path $repositoryRoot 'src\Nwflash.Desktop\src-tauri\Cargo.toml'
$verifySdk = Join-Path $PSScriptRoot 'verify-sdk.ps1'
$verifyLayout = Join-Path $PSScriptRoot 'verify-link-layout.ps1'

Push-Location $repositoryRoot
try {
    $cargoOutput = (& cargo test --manifest-path $manifestPath -p nwflash-protection --test vmp_build_contract 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw 'Rust SDK identity and archive contract tests failed.'
    }

    $cargoOutput += (& cargo test --manifest-path $manifestPath -p nwflash-protection --test vmp_probe bogus_sdk_root_is_ignored_when_feature_is_disabled -- --ignored --exact 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw 'No-feature bogus-root contract failed.'
    }

    $cargoOutput += (& cargo test --manifest-path $manifestPath -p nwflash-protection --test vmp_probe relative_sdk_root_feature_build_fails_before_filesystem_access -- --ignored --exact 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw 'Feature-build fully-qualified-root contract failed.'
    }

    $relativeRoot = [System.IO.Path]::GetRelativePath($repositoryRoot, $SdkRoot)
    $relativeRejected = $false
    try {
        & $verifySdk -SdkRoot $relativeRoot *> $null
    }
    catch {
        $relativeRejected = $_.Exception.Message.Contains('fully qualified')
    }
    if (-not $relativeRejected) {
        throw 'verify-sdk.ps1 accepted a relative SDK root or returned the wrong error.'
    }

    $sdkResult = (& $verifySdk -SdkRoot $SdkRoot -AsJson | ConvertFrom-Json)
    if (-not [bool]$sdkResult.verified -or [string]$sdkResult.machine -ne 'AMD64' -or
        [string]$sdkResult.sdk_dll_identity -ne 'VMProtectSDK64.dll' -or
        [int]$sdkResult.required_symbol_count -ne 8 -or [int]$sdkResult.files_copied -ne 0) {
        throw 'SDK verifier structured result did not prove the exact AMD64 SDK contract.'
    }

    $layoutResult = (& $verifyLayout -SdkRoot $SdkRoot -AsJson | ConvertFrom-Json)
    if (-not [bool]$layoutResult.verified -or [string]$layoutResult.machine -ne 'AMD64' -or
        [string]$layoutResult.imported_dll -ne 'VMProtectSDK64.dll' -or
        @($layoutResult.required_imports).Count -ne 8 -or [int]$layoutResult.files_copied -ne 0) {
        throw 'Link/layout verifier structured result did not prove the exact import contract.'
    }
    $expectedMarkers = @{
        nwflash_protection_accept_login_lease = 'Ultra'
        nwflash_protection_classify_heartbeat_lease = 'Virtualization'
        nwflash_protection_admit_local_operation = 'Ultra'
        nwflash_protection_verify_image_integrity = 'Virtualization'
        nwflash_protection_build_identity_matches = 'Mutation'
    }
    if (@($layoutResult.markers).Count -ne $expectedMarkers.Count) { throw 'Structured link result has the wrong marker count.' }
    foreach ($marker in @($layoutResult.markers)) {
        if (-not $expectedMarkers.ContainsKey([string]$marker.symbol) -or
            [string]$marker.mode -ne [string]$expectedMarkers[[string]$marker.symbol] -or
            -not [bool]$marker.verified -or [int]$marker.begin_count -ne 1 -or [int]$marker.end_count -ne 1) {
            throw "Structured marker result is invalid for $($marker.symbol)."
        }
    }

    if ($AsJson) {
        [ordered]@{
            schema = 1
            verified = $true
            sdk = $sdkResult
            link_layout = $layoutResult
        } | ConvertTo-Json -Depth 12 -Compress
    }
    else {
        Write-Output 'All VMProtect SDK and marker layout contracts passed.'
    }
}
finally {
    Pop-Location
}
