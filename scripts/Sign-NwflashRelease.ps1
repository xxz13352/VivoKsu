[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Path)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($env:NWFLASH_CERT_THUMBPRINT)) { throw 'NWFLASH_CERT_THUMBPRINT is required for Authenticode signing.' }
if (-not (Test-Path $Path)) { throw "Signing target is missing: $Path" }
$expectedThumbprint = ($env:NWFLASH_CERT_THUMBPRINT -replace '\s', '').ToUpperInvariant()
if ($expectedThumbprint -notmatch '^[0-9A-F]{40}$') {
    throw 'NWFLASH_CERT_THUMBPRINT must be a 40-character SHA-1 certificate thumbprint.'
}

$signTool = $env:NWFLASH_SIGNTOOL_PATH
if ([string]::IsNullOrWhiteSpace($signTool)) {
    $signTool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if ([string]::IsNullOrWhiteSpace($signTool) -or -not (Test-Path $signTool)) { throw 'SignTool executable is missing; set NWFLASH_SIGNTOOL_PATH.' }

& $signTool sign /sha1 $expectedThumbprint /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $Path
if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed with exit code $LASTEXITCODE." }
$signature = Get-AuthenticodeSignature $Path
if ($signature.Status -ne 'Valid') { throw "Authenticode signature is not valid: $($signature.Status)" }
$actualThumbprint = ($signature.SignerCertificate.Thumbprint -replace '\s', '').ToUpperInvariant()
if ($actualThumbprint -ne $expectedThumbprint) {
    throw "Authenticode signature certificate does not match NWFLASH_CERT_THUMBPRINT for $Path."
}
