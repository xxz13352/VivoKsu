[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$contractPath = Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1'
if (-not (Test-Path -LiteralPath $contractPath -PathType Leaf)) {
    throw "Protected release contract is missing: $contractPath"
}
. $contractPath

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-ThrowsLike {
    param([scriptblock]$Action, [string]$Pattern, [string]$Message)
    $rejected = $false
    try { & $Action } catch { $rejected = $_.Exception.Message -like $Pattern }
    Assert-Condition $rejected $Message
}

function New-Amd64PeFixture {
    param([Parameter(Mandatory)][string]$Path, [byte]$Tail = 0)
    $bytes = [byte[]]::new(512)
    $bytes[0] = 0x4D
    $bytes[1] = 0x5A
    [BitConverter]::GetBytes([int]0x80).CopyTo($bytes, 0x3C)
    $bytes[0x80] = 0x50
    $bytes[0x81] = 0x45
    [BitConverter]::GetBytes([uint16]0x8664).CopyTo($bytes, 0x84)
    $bytes[511] = $Tail
    [IO.File]::WriteAllBytes($Path, $bytes)
}

function New-ValidSignatureFixture {
    param([string]$Thumbprint = '00112233445566778899AABBCCDDEEFF00112233')
    [pscustomobject]@{
        Status = 'Valid'
        SignerCertificate = [pscustomobject]@{
            Thumbprint = $Thumbprint
            Subject = 'CN=NWFlash Test Signer'
            Issuer = 'CN=NWFlash Test CA'
            SerialNumber = '01020304'
        }
        TimeStamperCertificate = [pscustomobject]@{
            Thumbprint = 'FFEEDDCCBBAA99887766554433221100FFEEDDCC'
            Subject = 'CN=RFC3161 Test TSA'
            Issuer = 'CN=Test Timestamp CA'
            SerialNumber = 'A1B2C3D4'
        }
        StatusMessage = 'Signature verified.'
    }
}

function New-MarkerReviewFixture {
    param(
        [Parameter(Mandatory)][object]$Prepared,
        [Parameter(Mandatory)][string]$PreparedHash,
        [Parameter(Mandatory)][string]$CompilerLogHash
    )
    [ordered]@{
        schema = 1
        handoff_id = $Prepared.handoff_id
        prepared_manifest_sha256 = $PreparedHash
        compiler_log_sha256 = $CompilerLogHash
        vmprotect_edition = 'Lite'
        vmprotect_version = 'fixture-1.0'
        compiler_log_reviewed = $true
        compiler_log_has_errors = $false
        options = [ordered]@{
            memory_protection = $true
            import_protection = $true
            packing = $true
            vm_execution_denial = $false
        }
        markers = @(
            foreach ($marker in Get-NwflashProtectedMarkers) {
                [ordered]@{ name = $marker.name; mode = $marker.mode; compiled = $true }
            }
        )
        operator = 'fixture-operator'
        reviewed_utc = [DateTimeOffset]::UtcNow.ToString('o')
    }
}

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-task8-contract-" + [Guid]::NewGuid().ToString('N'))
$priorKey = $env:NWFLASH_SESSION_VERIFY_KEY_B64
$priorBuildId = $env:NWFLASH_BUILD_ID
$priorSdkRoot = $env:NWFLASH_VMP_SDK_ROOT
$priorThumbprint = $env:NWFLASH_CERT_THUMBPRINT

try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $sourceRoot = Join-Path $testRoot 'source'
    $sdkRoot = Join-Path $testRoot 'external-sdk-fixture'
    $handoffRoot = Join-Path $testRoot 'handoffs'
    $operatorRoot = Join-Path $testRoot 'operator-output'
    foreach ($directory in @($sourceRoot, $sdkRoot, $handoffRoot, $operatorRoot)) {
        New-Item -ItemType Directory -Path $directory | Out-Null
    }

    $sourceExe = Join-Path $sourceRoot 'nwflash-desktop.exe'
    $sourcePdb = Join-Path $sourceRoot 'nwflash-desktop.pdb'
    $sourceMap = Join-Path $sourceRoot 'nwflash-desktop.map'
    $protectedOutput = Join-Path $operatorRoot 'nwflash-desktop.protected.exe'
    $compilerLog = Join-Path $operatorRoot 'vmprotect-compiler.log'
    New-Amd64PeFixture -Path $sourceExe -Tail 1
    [IO.File]::WriteAllBytes($sourcePdb, [byte[]](1, 2, 3, 4))
    [IO.File]::WriteAllText($sourceMap, 'fixture marker layout')

    $env:NWFLASH_SESSION_VERIFY_KEY_B64 = [Convert]::ToBase64String([byte[]](0..31))
    $env:NWFLASH_BUILD_ID = 'task8.fixture:1'
    $env:NWFLASH_VMP_SDK_ROOT = $sdkRoot
    $env:NWFLASH_CERT_THUMBPRINT = '00112233445566778899AABBCCDDEEFF00112233'

    $unsignedSignature = [pscustomobject]@{
        Status = 'NotSigned'
        SignerCertificate = $null
        TimeStamperCertificate = $null
    }
    $operations = [pscustomobject]@{
        GetSignature = { param($Path) $unsignedSignature }
        AssertMatchingPdb = { param($Exe, $Pdb) }
        AssertMarkerLayout = { param($Exe, $Map) [pscustomobject]@{ verified = $true } }
        VerifySdk = { param($Sdk) [pscustomobject]@{ schema = 1; sdk_root = $Sdk; verified = $true } }
        VerifyLinkLayout = { param($Sdk) [pscustomobject]@{ schema = 1; sdk_root = $Sdk; verified = $true } }
        AssertNoVmProtectImports = { param($Path) }
        RunProbe = {
            param($Path, $Hash)
            [pscustomobject]@{
                exit_code = 0
                probe_available = $true
                VMProtectIsProtected = $true
                VMProtectIsValidImageCRC = $true
                observed_sha256 = $Hash
            }
        }
    }

    $environment = Assert-ProtectedBuildEnvironment
    Assert-Condition ($environment.build_id -eq 'task8.fixture:1') 'Validated build ID was not returned.'
    Assert-Condition ($environment.verification_key_sha256 -match '^[0-9A-F]{64}$') 'Key fingerprint must be uppercase SHA-256.'
    Assert-Condition (-not (($environment | ConvertTo-Json -Compress).Contains($env:NWFLASH_SESSION_VERIFY_KEY_B64))) 'Evidence exposed the verification key.'

    $savedKey = $env:NWFLASH_SESSION_VERIFY_KEY_B64
    Remove-Item Env:NWFLASH_SESSION_VERIFY_KEY_B64
    Assert-ThrowsLike { Assert-ProtectedBuildEnvironment } '*NWFLASH_SESSION_VERIFY_KEY_B64 is required*' 'Missing verification key was accepted.'
    $env:NWFLASH_SESSION_VERIFY_KEY_B64 = 'not-base64'
    Assert-ThrowsLike { Assert-ProtectedBuildEnvironment } '*must be standard Base64*' 'Malformed verification key was accepted.'
    $env:NWFLASH_SESSION_VERIFY_KEY_B64 = [Convert]::ToBase64String([byte[]](0..30))
    Assert-ThrowsLike { Assert-ProtectedBuildEnvironment } '*exactly 32 bytes*' 'Wrong verification-key length was accepted.'
    $env:NWFLASH_SESSION_VERIFY_KEY_B64 = $savedKey
    $savedBuildId = $env:NWFLASH_BUILD_ID
    $env:NWFLASH_BUILD_ID = 'contains spaces'
    Assert-ThrowsLike { Assert-ProtectedBuildEnvironment } '*must match*' 'Invalid build ID was accepted.'
    $env:NWFLASH_BUILD_ID = $savedBuildId
    $savedSdkRoot = $env:NWFLASH_VMP_SDK_ROOT
    $env:NWFLASH_VMP_SDK_ROOT = 'relative-sdk'
    Assert-ThrowsLike { Assert-ProtectedBuildEnvironment } '*fully qualified*' 'Relative SDK root was accepted.'
    $env:NWFLASH_VMP_SDK_ROOT = $savedSdkRoot

    Assert-ThrowsLike {
        Invoke-PrepareManualHandoffCore -InputExe $sourceExe -InputPdb $sourcePdb -InputMap $sourceMap `
            -ProtectedOutputPath $sourceExe -CompilerLogPath $compilerLog -HandoffRoot $handoffRoot -Operations $operations
    } '*must be distinct*' 'Prepare accepted in-place protected output.'

    $preparedPath = Invoke-PrepareManualHandoffCore -InputExe $sourceExe -InputPdb $sourcePdb -InputMap $sourceMap `
        -ProtectedOutputPath $protectedOutput -CompilerLogPath $compilerLog -HandoffRoot $handoffRoot -Operations $operations
    $prepared = Get-Content -Raw -LiteralPath $preparedPath | ConvertFrom-Json
    Assert-Condition ((Get-Item -LiteralPath $preparedPath).IsReadOnly) 'prepared.json must be immutable.'
    Assert-Condition ($prepared.state -eq 'prepared') 'Prepare emitted the wrong state.'
    Assert-Condition ($null -eq $prepared.previous_evidence_sha256) 'Prepared state must not invent prior evidence.'
    Assert-Condition ($prepared.input_exe.sha256 -eq (Get-Sha256Hex $sourceExe)) 'Prepared EXE hash does not bind the source.'
    Assert-Condition ($prepared.input_map.marker_layout_verified) 'Prepared evidence omitted desktop MAP proof.'
    Assert-Condition (-not (Get-ChildItem -LiteralPath (Split-Path -Parent $preparedPath) -Recurse -File | Where-Object { $_.Name -like 'VMProtect*' })) 'Prepare copied an SDK artifact.'

    $preparedHash = Get-Sha256Hex $preparedPath
    Copy-Item -LiteralPath $prepared.input_exe.path -Destination $protectedOutput
    (Get-Item -LiteralPath $protectedOutput).IsReadOnly = $false
    [IO.File]::WriteAllText($compilerLog, 'VMProtect compiler fixture: no errors')
    $compilerLogHash = Get-Sha256Hex $compilerLog
    $reviewPath = Join-Path $operatorRoot 'marker-review.json'
    New-MarkerReviewFixture -Prepared $prepared -PreparedHash $preparedHash -CompilerLogHash $compilerLogHash |
        ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reviewPath -Encoding UTF8

    Assert-ThrowsLike {
        Invoke-AcceptManualOutputCore -PreparedManifest $preparedPath -MarkerReviewPath $reviewPath -Operations $operations
    } '*must differ from the unprotected input*' 'Accept allowed unchanged protected bytes.'

    New-Amd64PeFixture -Path $protectedOutput -Tail 2
    $acceptedPath = Invoke-AcceptManualOutputCore -PreparedManifest $preparedPath -MarkerReviewPath $reviewPath -Operations $operations
    $accepted = Get-Content -Raw -LiteralPath $acceptedPath | ConvertFrom-Json
    Assert-Condition ((Get-Item -LiteralPath $acceptedPath).IsReadOnly) 'accepted.json must be immutable.'
    Assert-Condition ($accepted.state -eq 'accepted') 'Accept emitted the wrong state.'
    Assert-Condition ($accepted.previous_evidence_sha256 -eq $preparedHash) 'Accepted evidence is not hash-bound to prepared evidence.'
    Assert-Condition ($accepted.protected_output.sha256 -ne $accepted.input_exe_sha256) 'Accepted protected hash equals unprotected hash.'
    Assert-Condition ($accepted.is_protected -and $accepted.image_crc_valid) 'Accepted evidence omitted successful runtime oracles.'

    foreach ($exitCode in 41, 42, 43, 44) {
        Assert-ThrowsLike {
            Assert-ProtectedProbeResult -Result ([pscustomobject]@{
                exit_code = $exitCode
                probe_available = ($exitCode -notin 43, 44)
                VMProtectIsProtected = ($exitCode -notin 41, 43, 44)
                VMProtectIsValidImageCRC = ($exitCode -notin 42, 43, 44)
                observed_sha256 = $accepted.protected_output.sha256
            }) -ExpectedSha256 $accepted.protected_output.sha256
        } "*exit code $exitCode*" "Probe exit code $exitCode was accepted."
    }

    $validSignature = New-ValidSignatureFixture
    $identity = Assert-AuthenticodeIdentity -Signature $validSignature -ExpectedThumbprint $env:NWFLASH_CERT_THUMBPRINT
    Assert-Condition ($identity.timestamp_thumbprint -eq 'FFEEDDCCBBAA99887766554433221100FFEEDDCC') 'Timestamp identity was not captured.'
    Assert-ThrowsLike {
        Assert-AuthenticodeIdentity -Signature ([pscustomobject]@{ Status = 'NotSigned'; SignerCertificate = $null; TimeStamperCertificate = $null }) -ExpectedThumbprint $env:NWFLASH_CERT_THUMBPRINT
    } '*not valid*' 'Unsigned file was accepted as signed.'
    Assert-ThrowsLike {
        Assert-AuthenticodeIdentity -Signature (New-ValidSignatureFixture -Thumbprint '1111222233334444555566667777888899990000') -ExpectedThumbprint $env:NWFLASH_CERT_THUMBPRINT
    } '*thumbprint*' 'Wrong signing certificate was accepted.'
    $missingTimestamp = New-ValidSignatureFixture
    $missingTimestamp.TimeStamperCertificate = $null
    Assert-ThrowsLike {
        Assert-AuthenticodeIdentity -Signature $missingTimestamp -ExpectedThumbprint $env:NWFLASH_CERT_THUMBPRINT
    } '*RFC3161 timestamp*' 'Missing timestamp evidence was accepted.'

    Assert-ThrowsLike { Assert-ConsoleExecutable -Path (Join-Path $testRoot 'VMProtect.exe') } '*VMProtect_Con.exe*' 'Lite GUI was accepted as a console.'

    $releaseRoot = Join-Path $testRoot 'release-fixture'
    New-Item -ItemType Directory -Path (Join-Path $releaseRoot 'resources') | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $releaseRoot 'nwflash-desktop.exe'), [byte[]](1, 2, 3))
    [IO.File]::WriteAllBytes((Join-Path $releaseRoot 'app-setup.exe'), [byte[]](4, 5, 6))
    [IO.File]::WriteAllText((Join-Path $releaseRoot 'resources\approved.dll'), 'approved')
    [IO.File]::WriteAllText((Join-Path $releaseRoot '.nwflash-tauri-release'), 'marker')
    $allowedRelease = @('nwflash-desktop.exe', 'app-setup.exe', 'resources/approved.dll', '.nwflash-tauri-release')
    Assert-ExactFileSet -Root $releaseRoot -AllowedRelativePaths $allowedRelease
    foreach ($forbidden in @('debug.pdb', 'desktop.map', 'VMProtectSDK64.dll', 'compiler.log', 'extra.dll')) {
        $forbiddenPath = Join-Path $releaseRoot $forbidden
        [IO.File]::WriteAllText($forbiddenPath, 'forbidden')
        Assert-ThrowsLike { Assert-ExactFileSet -Root $releaseRoot -AllowedRelativePaths $allowedRelease } '*Unexpected file*' "Release allowlist accepted $forbidden."
        Remove-Item -LiteralPath $forbiddenPath
    }

    $installRoot = Join-Path $testRoot 'installed-fixture'
    New-Item -ItemType Directory -Path (Join-Path $installRoot 'resources') | Out-Null
    Copy-Item -LiteralPath (Join-Path $releaseRoot 'nwflash-desktop.exe') -Destination (Join-Path $installRoot 'nwflash-desktop.exe')
    [IO.File]::WriteAllText((Join-Path $installRoot 'uninstall.exe'), 'uninstaller')
    Copy-Item -LiteralPath (Join-Path $releaseRoot 'resources\approved.dll') -Destination (Join-Path $installRoot 'resources\approved.dll')
    Assert-InstalledTreeContract -InstallRoot $installRoot -ExpectedExeSha256 (Get-Sha256Hex (Join-Path $releaseRoot 'nwflash-desktop.exe')) `
        -ResourceHashes @{ 'resources/approved.dll' = (Get-Sha256Hex (Join-Path $releaseRoot 'resources\approved.dll')) } `
        -Signature $validSignature -ExpectedThumbprint $env:NWFLASH_CERT_THUMBPRINT
    [IO.File]::WriteAllText((Join-Path $installRoot 'unexpected.txt'), 'unexpected')
    Assert-ThrowsLike {
        Assert-InstalledTreeContract -InstallRoot $installRoot -ExpectedExeSha256 (Get-Sha256Hex (Join-Path $releaseRoot 'nwflash-desktop.exe')) `
            -ResourceHashes @{ 'resources/approved.dll' = (Get-Sha256Hex (Join-Path $releaseRoot 'resources\approved.dll')) } `
            -Signature $validSignature -ExpectedThumbprint $env:NWFLASH_CERT_THUMBPRINT
    } '*Unexpected file*' 'Installed-tree allowlist accepted an extra file.'

    $trace = [Collections.Generic.List[string]]::new()
    $pipelineOperations = [ordered]@{}
    foreach ($name in @('accept', 'copy', 'sign-exe', 'bundle', 'sign-installer', 'install-compare', 'verify', 'manifest', 'verify-final')) {
        $capturedName = $name
        $pipelineOperations[$name] = { $trace.Add($capturedName) | Out-Null }.GetNewClosure()
    }
    Invoke-ProtectedReleasePipeline -Operations $pipelineOperations
    Assert-Condition (($trace -join ',') -eq 'accept,copy,sign-exe,bundle,sign-installer,install-compare,verify,manifest,verify-final') 'Protected pipeline order is incorrect.'
    $trace.Clear()
    $pipelineOperations['bundle'] = { throw 'injected bundle failure' }
    Assert-ThrowsLike { Invoke-ProtectedReleasePipeline -Operations $pipelineOperations } '*injected bundle failure*' 'Injected pipeline failure did not fail closed.'
    Assert-Condition (($trace -join ',') -eq 'accept,copy,sign-exe') 'Pipeline continued after an injected failure.'

    Write-Host 'Protected release behavior contracts passed.'
}
finally {
    if ($null -eq $priorKey) { Remove-Item Env:NWFLASH_SESSION_VERIFY_KEY_B64 -ErrorAction SilentlyContinue } else { $env:NWFLASH_SESSION_VERIFY_KEY_B64 = $priorKey }
    if ($null -eq $priorBuildId) { Remove-Item Env:NWFLASH_BUILD_ID -ErrorAction SilentlyContinue } else { $env:NWFLASH_BUILD_ID = $priorBuildId }
    if ($null -eq $priorSdkRoot) { Remove-Item Env:NWFLASH_VMP_SDK_ROOT -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_SDK_ROOT = $priorSdkRoot }
    if ($null -eq $priorThumbprint) { Remove-Item Env:NWFLASH_CERT_THUMBPRINT -ErrorAction SilentlyContinue } else { $env:NWFLASH_CERT_THUMBPRINT = $priorThumbprint }
    Remove-ValidatedTemporaryRoot -Root $testRoot -Prefix 'nwflash-task8-contract-'
}
