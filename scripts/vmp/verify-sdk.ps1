[CmdletBinding()]
param(
    [Parameter()]
    [string]$SdkRoot = $env:NWFLASH_VMP_SDK_ROOT
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Amd64Machine = 0x8664
$RequiredDeclarations = @(
    'VMP_IMPORT void VMP_API VMProtectBeginVirtualization(const char *);',
    'VMP_IMPORT void VMP_API VMProtectBeginMutation(const char *);',
    'VMP_IMPORT void VMP_API VMProtectBeginUltra(const char *);',
    'VMP_IMPORT void VMP_API VMProtectEnd(void);',
    'VMP_IMPORT bool VMP_API VMProtectIsProtected();',
    'VMP_IMPORT bool VMP_API VMProtectIsDebuggerPresent(bool);',
    'VMP_IMPORT bool VMP_API VMProtectIsVirtualMachinePresent(void);',
    'VMP_IMPORT bool VMP_API VMProtectIsValidImageCRC(void);'
)

function Resolve-RequiredLeaf {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description is missing at the required path: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).ProviderPath
}

function Get-UInt16LittleEndian {
    param(
        [Parameter(Mandatory = $true)]
        [byte[]]$Bytes,
        [Parameter(Mandatory = $true)]
        [int]$Offset
    )

    if ($Offset -lt 0 -or $Offset + 2 -gt $Bytes.Length) {
        throw 'Unexpected end of binary while reading a 16-bit value.'
    }
    return ([int]$Bytes[$Offset] -bor ([int]$Bytes[$Offset + 1] -shl 8))
}

function Assert-Amd64CoffArchive {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    [byte[]]$bytes = [System.IO.File]::ReadAllBytes($Path)
    [byte[]]$magic = [System.Text.Encoding]::ASCII.GetBytes("!<arch>`n")
    if ($bytes.Length -lt $magic.Length) {
        throw "Import library is too short to be a COFF archive: $Path"
    }
    for ($index = 0; $index -lt $magic.Length; $index++) {
        if ($bytes[$index] -ne $magic[$index]) {
            throw "Import library is not a COFF archive: $Path"
        }
    }

    $offset = 8
    $objectCount = 0
    while ($offset -lt $bytes.Length) {
        if ($offset + 60 -gt $bytes.Length) {
            throw "Import library has a truncated archive header: $Path"
        }

        $name = [System.Text.Encoding]::ASCII.GetString($bytes, $offset, 16).Trim()
        $sizeText = [System.Text.Encoding]::ASCII.GetString($bytes, $offset + 48, 10).Trim()
        $memberSize = 0
        if (-not [int]::TryParse($sizeText, [ref]$memberSize) -or $memberSize -lt 0) {
            throw "Import library has an invalid archive member size: $Path"
        }
        if ($bytes[$offset + 58] -ne 0x60 -or $bytes[$offset + 59] -ne 0x0A) {
            throw "Import library has an invalid archive member header: $Path"
        }

        $dataOffset = $offset + 60
        $dataEnd = $dataOffset + $memberSize
        if ($dataEnd -gt $bytes.Length) {
            throw "Import library has a truncated archive member: $Path"
        }

        if ($name -ne '/' -and $name -ne '//') {
            if ($memberSize -lt 2) {
                throw "Import library contains an unrecognized COFF member: $Path"
            }
            $machine = Get-UInt16LittleEndian -Bytes $bytes -Offset $dataOffset
            if ($machine -eq 0) {
                if ($memberSize -lt 8 -or
                    (Get-UInt16LittleEndian -Bytes $bytes -Offset ($dataOffset + 2)) -ne 0xFFFF) {
                    throw "Import library contains an unrecognized COFF member: $Path"
                }
                $machine = Get-UInt16LittleEndian -Bytes $bytes -Offset ($dataOffset + 6)
            }
            if ($machine -ne $Amd64Machine) {
                throw ('Import library has wrong COFF architecture 0x{0:X4}; expected AMD64 0x8664: {1}' -f $machine, $Path)
            }
            $objectCount++
        }

        $offset = $dataEnd + ($memberSize % 2)
    }

    if ($objectCount -eq 0) {
        throw "Import library contains no AMD64 COFF objects: $Path"
    }
}

function Assert-Amd64PeImage {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    [byte[]]$bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 0x40 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "SDK DLL is not a PE image: $Path"
    }
    $peOffset = [System.BitConverter]::ToInt32($bytes, 0x3C)
    if ($peOffset -lt 0 -or $peOffset + 6 -gt $bytes.Length -or
        $bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or
        $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
        throw "SDK DLL has an invalid PE header: $Path"
    }
    $machine = Get-UInt16LittleEndian -Bytes $bytes -Offset ($peOffset + 4)
    if ($machine -ne $Amd64Machine) {
        throw ('SDK DLL has wrong PE architecture 0x{0:X4}; expected AMD64 0x8664: {1}' -f $machine, $Path)
    }
}

if ([string]::IsNullOrWhiteSpace($SdkRoot)) {
    throw 'NWFLASH_VMP_SDK_ROOT is required and must point to the external VMProtect package root.'
}
if (-not (Test-Path -LiteralPath $SdkRoot -PathType Container)) {
    throw "VMProtect SDK root does not exist: $SdkRoot"
}

$resolvedRoot = (Resolve-Path -LiteralPath $SdkRoot).ProviderPath
$headerPath = Resolve-RequiredLeaf -Path (Join-Path $resolvedRoot 'Include\C\VMProtectSDK.h') -Description 'VMProtect C header'
$libraryPath = Resolve-RequiredLeaf -Path (Join-Path $resolvedRoot 'Lib\Windows\VMProtectSDK64.lib') -Description 'VMProtect x64 import library'
$dllPath = Resolve-RequiredLeaf -Path (Join-Path $resolvedRoot 'Lib\Windows\VMProtectSDK64.dll') -Description 'VMProtect x64 SDK DLL'

$normalizedHeader = ((Get-Content -LiteralPath $headerPath -Raw) -replace '\s+', ' ').Trim()
foreach ($declaration in $RequiredDeclarations) {
    if (-not $normalizedHeader.Contains($declaration)) {
        throw "VMProtect header is missing required declaration '$declaration': $headerPath"
    }
}

Assert-Amd64CoffArchive -Path $libraryPath
Assert-Amd64PeImage -Path $dllPath

Write-Output "VMProtect SDK root: $resolvedRoot"
Write-Output "Header: $headerPath"
Write-Output "Import library: $libraryPath (AMD64 COFF)"
Write-Output "SDK DLL: $dllPath (AMD64 PE)"
Write-Output 'VMProtect SDK validation passed. No files were copied or modified.'
