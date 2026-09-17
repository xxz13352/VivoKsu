#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Path,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{64}$')][string]$ExpectedUnsignedSha256,
    [Parameter(Mandatory)][string]$InputEvidence,
    [Parameter(Mandatory)][string]$SigningEvidenceOut,
    [Parameter(Mandatory)][ValidateSet('exe-signed', 'installer-signed')][string]$State
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')

$target = Resolve-FullyQualifiedLeaf $Path
$evidencePath = Resolve-FullyQualifiedLeaf $InputEvidence
if ($State -eq 'exe-signed') {
    $acceptedChain = Assert-AcceptedEvidenceChain -AcceptedEvidence $evidencePath -Operations (New-DefaultProtectionOperations)
    $inputDocument = $acceptedChain.accepted
    if ($target.Equals([string]$acceptedChain.protected_output, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Signing must operate on a distinct packaging copy, never the immutable accepted output.'
    }
    if ($ExpectedUnsignedSha256.ToUpperInvariant() -ne [string]$inputDocument.protected_output.sha256) {
        throw 'EXE signing hash is not bound to the accepted protected output.'
    }
}
else {
    $nsisChain = Assert-InstallerSigningTarget -Target $target -ExpectedUnsignedSha256 $ExpectedUnsignedSha256 `
        -NsisEvidence $evidencePath -Operations (New-DefaultProtectionOperations)
    $inputDocument = $nsisChain.nsis
}
$inputEvidenceHash = Get-Sha256Hex $evidencePath
$unsignedHash = Get-Sha256Hex $target
if ($unsignedHash -ne $ExpectedUnsignedSha256.ToUpperInvariant()) {
    throw 'Signing target hash does not equal the evidence-bound unsigned hash.'
}
$beforeSignature = Get-AuthenticodeSignature -LiteralPath $target
if ([string]$beforeSignature.Status -ne 'NotSigned') {
    throw "Signing target must be unsigned; status was $($beforeSignature.Status)."
}

if ([string]::IsNullOrWhiteSpace($env:NWFLASH_CERT_THUMBPRINT)) {
    throw 'NWFLASH_CERT_THUMBPRINT is required only when Authenticode signing begins.'
}
$expectedThumbprint = ($env:NWFLASH_CERT_THUMBPRINT -replace '\s', '').ToUpperInvariant()
if ($expectedThumbprint -notmatch '^[0-9A-F]{40}$') {
    throw 'NWFLASH_CERT_THUMBPRINT must be a 40-character SHA-1 certificate thumbprint.'
}

$signTool = $env:NWFLASH_SIGNTOOL_PATH
if ([string]::IsNullOrWhiteSpace($signTool)) {
    $signTool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -File -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '(?i)\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
$signTool = Resolve-FullyQualifiedLeaf $signTool
if ($signTool -notmatch '(?i)\\x64\\signtool\.exe$') { throw 'Signing requires x64 SignTool.' }

$signArguments = @('sign', '/sha1', $expectedThumbprint, '/fd', 'SHA256', '/tr', 'https://timestamp.digicert.com', '/td', 'SHA256', $target)
$signOutput = (& $signTool @signArguments 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed with exit code $LASTEXITCODE." }
$verifyOutput = (& $signTool verify /pa /all /v $target 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { throw "SignTool verification failed with exit code $LASTEXITCODE." }
if ($verifyOutput -notmatch '(?i)timestamp') { throw 'SignTool verification did not report timestamp evidence.' }

$signature = Get-AuthenticodeSignature -LiteralPath $target
$identity = Assert-AuthenticodeIdentity -Signature $signature -ExpectedThumbprint $expectedThumbprint
$signedHash = Get-Sha256Hex $target
if ($signedHash -eq $unsignedHash) { throw 'Authenticode signing did not change the target hash.' }
$verificationBytes = [Text.UTF8Encoding]::new($false).GetBytes($signOutput + $verifyOutput)
$document = [ordered]@{
    schema = 1
    handoff_id = [string]$inputDocument.handoff_id
    state = $State
    created_utc = [DateTimeOffset]::UtcNow.ToString('o')
    previous_evidence_sha256 = $inputEvidenceHash
    input_evidence_sha256 = $inputEvidenceHash
    input_evidence_path = $evidencePath
    target_path = $target
    unsigned_sha256 = $unsignedHash
    signed_sha256 = $signedHash
    certificate = $identity
    digest_algorithm = 'SHA256'
    timestamp_protocol = 'RFC3161'
    timestamp_url = 'https://timestamp.digicert.com'
    timestamp_digest_algorithm = 'SHA256'
    signtool_verification_sha256 = Get-BytesSha256Hex $verificationBytes
}
Write-AtomicEvidence -Path $SigningEvidenceOut -Value $document
