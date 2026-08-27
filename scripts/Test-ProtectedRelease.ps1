#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$contractPath = Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1'
if (-not (Test-Path -LiteralPath $contractPath -PathType Leaf)) {
    throw "Protected release contract is missing: $contractPath"
}
& (Join-Path $PSScriptRoot 'Test-PowerShellRuntimeBoundary.ps1')
. $contractPath

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-ThrowsLike {
    param([scriptblock]$Action, [string]$Pattern, [string]$Message)
    $rejected = $false
    $observed = '<no exception>'
    try { & $Action } catch {
        $observed = $_.Exception.Message
        $rejected = $observed -like $Pattern
    }
    Assert-Condition $rejected "$Message Observed: $observed"
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
        [Parameter(Mandatory)][string]$CompilerLogHash,
        [Parameter(Mandatory)][string]$ProtectedOutputHash
    )
    [ordered]@{
        schema = 1
        handoff_id = $Prepared.handoff_id
        prepared_manifest_sha256 = $PreparedHash
        compiler_log_sha256 = $CompilerLogHash
        protected_output_sha256 = $ProtectedOutputHash
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
    $sdkFixture = [pscustomobject]@{
        schema = 1
        verified = $true
        machine = 'AMD64'
        sdk_dll_identity = 'VMProtectSDK64.dll'
        required_symbols = @(Get-NwflashRequiredSdkImports)
        required_symbol_count = 8
        header_sha256 = '2300B7B4BB6BBF9CFA08013EC2D9B2FDCEB3DFD2E603CD1E24A493DE4D165B15'
        import_library_sha256 = '9997A9C6E179010450385832A66EA36938E180FC9067D91FD6AAE7C9F6BF4D18'
        sdk_dll_sha256 = 'EC3235136A4DAEE2A6F72C0F2994A8365CA8427C8068D068130B74C9FA64CD02'
        files_copied = 0
    }
    $linkFixture = [pscustomobject]@{
        schema = 1
        verified = $true
        sdk = $sdkFixture
        link_layout = [pscustomobject]@{
            schema = 1
            verified = $true
            machine = 'AMD64'
            imported_dll = 'VMProtectSDK64.dll'
            required_imports = @(Get-NwflashRequiredSdkImports)
            markers = @(foreach ($marker in Get-NwflashProtectedMarkers) {
                [pscustomobject]@{
                    symbol = $marker.symbol
                    mode = $marker.mode
                    begin_count = 1
                    end_count = 1
                    verified = $true
                }
            })
            files_copied = 0
        }
    }
    $operations = [pscustomobject]@{
        GetSignature = { param($Path) $unsignedSignature }
        AssertMatchingPdb = { param($Exe, $Pdb) }
        AssertMarkerLayout = { param($Exe, $Map) [pscustomobject]@{ verified = $true } }
        AssertExpectedVmProtectImports = {
            param($Path)
            [pscustomobject]@{
                verified = $true
                imported_dll = 'VMProtectSDK64.dll'
                required_imports = @(Get-NwflashRequiredSdkImports)
            }
        }
        VerifySdk = { param($Sdk) $sdkFixture }.GetNewClosure()
        VerifyLinkLayout = { param($Sdk) $linkFixture }.GetNewClosure()
        AssertNoVmProtectImports = { param($Path) }
        RunProbe = {
            param($Path, $Hash)
            [pscustomobject]@{
                exit_code = 0
                probe_available = $true
                VMProtectIsProtected = $true
                VMProtectIsValidImageCRC = $true
                observed_sha256 = $Hash
                build_id = $env:NWFLASH_BUILD_ID
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
    New-MarkerReviewFixture -Prepared $prepared -PreparedHash $preparedHash -CompilerLogHash $compilerLogHash `
        -ProtectedOutputHash (Get-Sha256Hex $protectedOutput) |
        ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reviewPath -Encoding UTF8

    Assert-ThrowsLike {
        Invoke-AcceptManualOutputCore -PreparedManifest $preparedPath -MarkerReviewPath $reviewPath -Operations $operations
    } '*must differ from the unprotected input*' 'Accept allowed unchanged protected bytes.'

    New-Amd64PeFixture -Path $protectedOutput -Tail 2
    New-MarkerReviewFixture -Prepared $prepared -PreparedHash $preparedHash -CompilerLogHash $compilerLogHash `
        -ProtectedOutputHash (Get-Sha256Hex $protectedOutput) |
        ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reviewPath -Encoding UTF8
    $acceptedPath = Invoke-AcceptManualOutputCore -PreparedManifest $preparedPath -MarkerReviewPath $reviewPath -Operations $operations
    $accepted = Get-Content -Raw -LiteralPath $acceptedPath | ConvertFrom-Json
    Assert-Condition ((Get-Item -LiteralPath $acceptedPath).IsReadOnly) 'accepted.json must be immutable.'
    Assert-Condition ($accepted.state -eq 'accepted') 'Accept emitted the wrong state.'
    Assert-Condition ($accepted.previous_evidence_sha256 -eq $preparedHash) 'Accepted evidence is not hash-bound to prepared evidence.'
    Assert-Condition ($accepted.protected_output.sha256 -ne $accepted.input_exe_sha256) 'Accepted protected hash equals unprotected hash.'
    Assert-Condition ($accepted.is_protected -and $accepted.image_crc_valid) 'Accepted evidence omitted successful runtime oracles.'
    $acceptedChain = Assert-AcceptedEvidenceChain -AcceptedEvidence $acceptedPath -Operations $operations
    Assert-Condition ($acceptedChain.protected_output -eq $protectedOutput) 'Accepted evidence full-chain validation returned the wrong output.'

    $forgedAccepted = Get-Content -Raw -LiteralPath $acceptedPath | ConvertFrom-Json -AsHashtable
    $forgedAccepted.protected_output.path = [string]$prepared.input_exe.path
    $forgedAccepted.protected_output.length = [long]$prepared.input_exe.length
    $forgedAccepted.protected_output.sha256 = [string]$prepared.input_exe.sha256
    $forgedPath = Write-AtomicEvidence -Path (Join-Path (Split-Path -Parent $acceptedPath) 'forged-accepted.json') -Value $forgedAccepted
    Assert-ThrowsLike {
        Assert-AcceptedEvidenceChain -AcceptedEvidence $forgedPath -Operations $operations
    } '*protected output*' 'A self-consistent forged accepted document authorized the unprotected input.'

    $signingRoot = Join-Path $testRoot 'signing-preflight'
    $signingEvidenceRoot = Join-Path $signingRoot 'evidence'
    $signingReleaseRoot = Join-Path $signingRoot 'release'
    New-Item -ItemType Directory -Path $signingEvidenceRoot,$signingReleaseRoot | Out-Null
    $packagingExe = Join-Path $signingReleaseRoot 'nwflash-desktop.exe'
    Copy-Item -LiteralPath $protectedOutput -Destination $packagingExe
    (Get-Item -LiteralPath $packagingExe).IsReadOnly = $false
    [IO.File]::WriteAllBytes($packagingExe, [byte[]]((Get-Content -AsByteStream -Raw -LiteralPath $packagingExe) + 7))
    $exeEvidencePath = Write-AtomicEvidence -Path (Join-Path $signingEvidenceRoot 'exe-signed.json') -Value ([ordered]@{
        schema = 1
        handoff_id = [string]$accepted.handoff_id
        state = 'exe-signed'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = Get-Sha256Hex $acceptedPath
        input_evidence_sha256 = Get-Sha256Hex $acceptedPath
        input_evidence_path = $acceptedPath
        target_path = $packagingExe
        unsigned_sha256 = [string]$accepted.protected_output.sha256
        signed_sha256 = Get-Sha256Hex $packagingExe
    })
    $installer = Join-Path $signingReleaseRoot 'nwflash-setup.exe'
    [IO.File]::WriteAllBytes($installer, [byte[]](9, 8, 7, 6))
    $installerHash = Get-Sha256Hex $installer
    $nsisEvidencePath = Write-AtomicEvidence -Path (Join-Path $signingEvidenceRoot 'nsis-built.json') -Value ([ordered]@{
        schema = 1
        handoff_id = [string]$accepted.handoff_id
        state = 'nsis-built'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = Get-Sha256Hex $exeEvidencePath
        signed_exe_sha256 = Get-Sha256Hex $packagingExe
        installer_path = $installer
        installer_unsigned_sha256 = $installerHash
    })
    $installerChain = Assert-InstallerSigningTarget -Target $installer -ExpectedUnsignedSha256 $installerHash `
        -NsisEvidence $nsisEvidencePath -Operations $operations
    Assert-Condition ($installerChain.installer -eq $installer) 'Installer signing preflight returned the wrong target.'
    $otherInstaller = Join-Path $signingReleaseRoot 'other-setup.exe'
    [IO.File]::WriteAllBytes($otherInstaller, [byte[]](1, 1, 1, 1))
    Assert-ThrowsLike {
        Assert-InstallerSigningTarget -Target $otherInstaller -ExpectedUnsignedSha256 (Get-Sha256Hex $otherInstaller) `
            -NsisEvidence $nsisEvidencePath -Operations $operations
    } '*target path*' 'Installer signing accepted a target outside nsis-built evidence.'
    Assert-ThrowsLike {
        Assert-InstallerSigningTarget -Target $installer -ExpectedUnsignedSha256 ('0' * 64) `
            -NsisEvidence $nsisEvidencePath -Operations $operations
    } '*hash*' 'Installer signing accepted a hash outside nsis-built evidence.'

    foreach ($exitCode in 41, 42, 43, 44) {
        Assert-ThrowsLike {
            Assert-ProtectedProbeResult -Result ([pscustomobject]@{
                exit_code = $exitCode
                probe_available = ($exitCode -notin 43, 44)
                VMProtectIsProtected = ($exitCode -notin 41, 43, 44)
                VMProtectIsValidImageCRC = ($exitCode -notin 42, 43, 44)
                observed_sha256 = $accepted.protected_output.sha256
                build_id = $env:NWFLASH_BUILD_ID
            }) -ExpectedSha256 $accepted.protected_output.sha256
        } "*exit code $exitCode*" "Probe exit code $exitCode was accepted."
    }
    Assert-ThrowsLike {
        Assert-ProtectedProbeResult -Result ([pscustomobject]@{
            exit_code = 0
            probe_available = $true
            VMProtectIsProtected = $true
            VMProtectIsValidImageCRC = $true
            observed_sha256 = $accepted.protected_output.sha256
            build_id = 'different-build'
        }) -ExpectedSha256 $accepted.protected_output.sha256 -ExpectedBuildId $env:NWFLASH_BUILD_ID
    } '*build identity*' 'Protected probe accepted an output from a different build.'

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

    Assert-ThrowsLike { Assert-ConsoleExecutable -Path (Join-Path $testRoot 'VMProtect_Con.exe') } '*disabled*' 'Automated VMProtect console execution was enabled.'

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

    if ($IsWindows) {
        $reparseRoot = Join-Path $testRoot 'reparse-allowlist'
        $reparseTarget = Join-Path $testRoot 'reparse-target'
        New-Item -ItemType Directory -Path $reparseRoot | Out-Null
        New-Item -ItemType Directory -Path $reparseTarget | Out-Null
        [IO.File]::WriteAllText((Join-Path $reparseRoot 'marker.txt'), 'marker')
        [IO.File]::WriteAllText((Join-Path $reparseTarget 'external.dll'), 'external')
        $junction = Join-Path $reparseRoot 'resources'
        New-Item -ItemType Junction -Path $junction -Target $reparseTarget | Out-Null
        try {
            Assert-ThrowsLike {
                Assert-ExactFileSet -Root $reparseRoot -AllowedRelativePaths @('marker.txt')
            } '*Reparse points*' 'Release allowlist traversed a junction.'
        }
        finally {
            if (Test-Path -LiteralPath $junction) { Remove-Item -LiteralPath $junction -Force }
        }

        $cleanupTarget = Join-Path $testRoot 'cleanup-target'
        New-Item -ItemType Directory -Path $cleanupTarget | Out-Null
        [IO.File]::WriteAllText((Join-Path $cleanupTarget 'must-survive.txt'), 'survive')
        $cleanupJunction = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-cleanup-guard-' + [Guid]::NewGuid().ToString('N'))
        New-Item -ItemType Junction -Path $cleanupJunction -Target $cleanupTarget | Out-Null
        try {
            Assert-ThrowsLike {
                Initialize-VerifiedInstallRoot -Path $cleanupJunction
            } '*Reparse*' 'Installer preflight accepted a junction install root.'
            Assert-ThrowsLike {
                Remove-ValidatedTemporaryRoot -Root $cleanupJunction -Prefix 'nwflash-cleanup-guard-'
            } '*reparse*' 'Validated cleanup accepted a junction root.'
            Assert-Condition (Test-Path -LiteralPath (Join-Path $cleanupTarget 'must-survive.txt')) 'Junction cleanup touched the external target.'
        }
        finally {
            if (Test-Path -LiteralPath $cleanupJunction) { Remove-Item -LiteralPath $cleanupJunction -Force }
        }
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
