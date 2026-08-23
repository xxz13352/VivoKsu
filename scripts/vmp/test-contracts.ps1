[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SdkRoot
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
    & cargo test --manifest-path $manifestPath -p nwflash-protection --test vmp_build_contract
    if ($LASTEXITCODE -ne 0) {
        throw 'Rust SDK identity and archive contract tests failed.'
    }

    & cargo test --manifest-path $manifestPath -p nwflash-protection --test vmp_probe bogus_sdk_root_is_ignored_when_feature_is_disabled -- --ignored --exact
    if ($LASTEXITCODE -ne 0) {
        throw 'No-feature bogus-root contract failed.'
    }

    & cargo test --manifest-path $manifestPath -p nwflash-protection --test vmp_probe relative_sdk_root_feature_build_fails_before_filesystem_access -- --ignored --exact
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

    $sdkOutput = (& $verifySdk -SdkRoot $SdkRoot 2>&1 | Out-String)
    if (-not $sdkOutput.Contains('Required DLL exports: verified')) {
        throw "SDK verifier did not prove DLL export identity: $sdkOutput"
    }

    $layoutOutput = (& $verifyLayout -SdkRoot $SdkRoot 2>&1 | Out-String)
    foreach ($requiredLine in @(
        'Final PE imports: VMProtectSDK64.dll and 8 required symbols verified',
        'Marker region: nwflash_protection_accept_login_lease = Ultra + End',
        'Marker region: nwflash_protection_classify_heartbeat_lease = Virtualization + End',
        'Marker region: nwflash_protection_admit_local_operation = Ultra + End',
        'Marker region: nwflash_protection_verify_image_integrity = Virtualization + End',
        'Marker region: nwflash_protection_build_identity_matches = Mutation + End'
    )) {
        if (-not $layoutOutput.Contains($requiredLine)) {
            throw "Link/layout verifier omitted '$requiredLine': $layoutOutput"
        }
    }

    Write-Output 'All VMProtect SDK and marker layout contracts passed.'
}
finally {
    Pop-Location
}
