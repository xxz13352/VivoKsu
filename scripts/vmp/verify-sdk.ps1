#requires -Version 7.4
#requires -PSEdition Core

[CmdletBinding()]
param(
    [Parameter()]
    [string]$SdkRoot = $env:NWFLASH_VMP_SDK_ROOT,
    [switch]$AsJson
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Amd64Machine = 0x8664
$ExpectedSdkDll = 'VMProtectSDK64.dll'
$ExpectedHeaderSha256 = '2300B7B4BB6BBF9CFA08013EC2D9B2FDCEB3DFD2E603CD1E24A493DE4D165B15'
$ExpectedImportLibrarySha256 = '9997A9C6E179010450385832A66EA36938E180FC9067D91FD6AAE7C9F6BF4D18'
$ExpectedSdkDllSha256 = 'EC3235136A4DAEE2A6F72C0F2994A8365CA8427C8068D068130B74C9FA64CD02'
$RequiredSymbols = @(
    'VMProtectBeginVirtualization',
    'VMProtectBeginMutation',
    'VMProtectBeginUltra',
    'VMProtectEnd',
    'VMProtectIsProtected',
    'VMProtectIsDebuggerPresent',
    'VMProtectIsVirtualMachinePresent',
    'VMProtectIsValidImageCRC'
)
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
    $resolved = (Resolve-Path -LiteralPath $Path).ProviderPath
    Assert-NoReparseAncestors -Path $resolved
    return $resolved
}

function Assert-NoReparseAncestors {
    param([Parameter(Mandatory)][string]$Path)

    $cursor = [IO.Path]::GetFullPath($Path)
    while (-not [string]::IsNullOrWhiteSpace($cursor)) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
                [string]$item.LinkType -in @('SymbolicLink', 'Junction')) {
                throw "VMProtect SDK paths may not traverse a reparse point: $cursor"
            }
        }
        $parent = Split-Path -Parent $cursor
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $cursor) { break }
        $cursor = $parent
    }
}

function Get-UInt16LittleEndian {
    param([byte[]]$Bytes, [int]$Offset)
    if ($Offset -lt 0 -or $Offset + 2 -gt $Bytes.Length) {
        throw 'Unexpected end of binary while reading a 16-bit value.'
    }
    return ([int]$Bytes[$Offset] -bor ([int]$Bytes[$Offset + 1] -shl 8))
}

function Get-UInt32LittleEndian {
    param([byte[]]$Bytes, [int]$Offset)
    if ($Offset -lt 0 -or $Offset + 4 -gt $Bytes.Length) {
        throw 'Unexpected end of binary while reading a 32-bit value.'
    }
    return [uint32]([uint32]$Bytes[$Offset] -bor
        ([uint32]$Bytes[$Offset + 1] -shl 8) -bor
        ([uint32]$Bytes[$Offset + 2] -shl 16) -bor
        ([uint32]$Bytes[$Offset + 3] -shl 24))
}

function Read-NullTerminatedAscii {
    param(
        [byte[]]$Bytes,
        [int]$Offset,
        [int]$Limit,
        [string]$Description
    )
    if ($Offset -lt 0 -or $Offset -ge $Limit -or $Limit -gt $Bytes.Length) {
        throw "Invalid $Description range."
    }
    $end = $Offset
    while ($end -lt $Limit -and $Bytes[$end] -ne 0) {
        $end++
    }
    if ($end -eq $Limit) {
        throw "$Description is missing a null terminator."
    }
    $value = [System.Text.Encoding]::ASCII.GetString($Bytes, $Offset, $end - $Offset)
    if ([string]::IsNullOrEmpty($value)) {
        throw "$Description is empty."
    }
    return [pscustomobject]@{ Value = $value; NextOffset = $end + 1 }
}

function Assert-Amd64SdkImportArchive {
    param([string]$Path)

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

    $imports = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
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
            $isShortImport = $machine -eq 0 -and $memberSize -ge 8 -and
                (Get-UInt16LittleEndian -Bytes $bytes -Offset ($dataOffset + 2)) -eq 0xFFFF
            if ($isShortImport) {
                $machine = Get-UInt16LittleEndian -Bytes $bytes -Offset ($dataOffset + 6)
            }
            if ($machine -ne $Amd64Machine) {
                throw ('Import library has wrong COFF architecture 0x{0:X4}; expected AMD64 0x8664: {1}' -f $machine, $Path)
            }
            $objectCount++

            if ($isShortImport) {
                if ($memberSize -lt 20) {
                    throw "Import library contains a truncated short import header: $Path"
                }
                $payloadSize = Get-UInt32LittleEndian -Bytes $bytes -Offset ($dataOffset + 12)
                $payloadOffset = $dataOffset + 20
                $payloadEnd = $payloadOffset + $payloadSize
                if ($payloadEnd -gt $dataEnd) {
                    throw "Import library contains a truncated short import payload: $Path"
                }
                $symbol = Read-NullTerminatedAscii -Bytes $bytes -Offset $payloadOffset -Limit $payloadEnd -Description 'import symbol'
                $dll = Read-NullTerminatedAscii -Bytes $bytes -Offset $symbol.NextOffset -Limit $payloadEnd -Description 'imported DLL name'
                if (-not $dll.Value.Equals($ExpectedSdkDll, [System.StringComparison]::OrdinalIgnoreCase)) {
                    throw "Import library maps $($symbol.Value) to $($dll.Value); expected $ExpectedSdkDll"
                }
                [void]$imports.Add($symbol.Value)
            }
        }

        if (($memberSize % 2) -eq 1) {
            if ($dataEnd -ge $bytes.Length) {
                throw "Import library is missing odd-member archive padding: $Path"
            }
            if ($bytes[$dataEnd] -ne 0x0A) {
                throw "Import library has invalid odd-member archive padding: $Path"
            }
            $offset = $dataEnd + 1
        }
        else {
            $offset = $dataEnd
        }
    }

    if ($objectCount -eq 0) {
        throw "Import library contains no AMD64 COFF objects: $Path"
    }
    foreach ($symbol in $RequiredSymbols) {
        if (-not $imports.Contains($symbol)) {
            throw "Import library is missing required symbol $symbol for ${ExpectedSdkDll}: $Path"
        }
    }
}

function Assert-Amd64PeImage {
    param([string]$Path)

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

function Resolve-X64Dumpbin {
    $vswhereCandidates = @(
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe",
        "$env:ProgramFiles\Microsoft Visual Studio\Installer\vswhere.exe"
    )
    $vswhere = $vswhereCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (-not $vswhere) {
        throw 'vswhere.exe was not found; cannot validate SDK DLL exports.'
    }
    $installation = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
    $vswhereSucceeded = $?
    if (-not $vswhereSucceeded -or [string]::IsNullOrWhiteSpace($installation)) {
        throw 'vswhere.exe did not locate Visual Studio C++ x64 tools.'
    }
    $toolsRoot = Join-Path $installation.Trim() 'VC\Tools\MSVC'
    $dumpbin = Get-ChildItem -LiteralPath $toolsRoot -Directory |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'bin\Hostx64\x64\dumpbin.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    if (-not $dumpbin) {
        throw "Visual Studio x64 dumpbin.exe was not found under $toolsRoot"
    }
    return (Resolve-Path -LiteralPath $dumpbin).ProviderPath
}

function Assert-RequiredDllExports {
    param([string]$Dumpbin, [string]$DllPath)

    $output = (& $Dumpbin /NOLOGO /EXPORTS $DllPath 2>&1 | Out-String)
    $dumpbinSucceeded = $?
    if (-not $dumpbinSucceeded) {
        throw "dumpbin failed to inspect SDK DLL exports: $output"
    }
    if (-not $output.Contains("exports for $ExpectedSdkDll")) {
        throw "SDK DLL export table identity is not $ExpectedSdkDll"
    }
    foreach ($symbol in $RequiredSymbols) {
        if ($output -notmatch "(?m)^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]+\s+$([regex]::Escape($symbol))\s*$") {
            throw "SDK DLL is missing required export $symbol"
        }
    }
}

if ([string]::IsNullOrWhiteSpace($SdkRoot)) {
    throw 'NWFLASH_VMP_SDK_ROOT is required and must point to the external VMProtect package root.'
}
if (-not [System.IO.Path]::IsPathFullyQualified($SdkRoot)) {
    throw 'NWFLASH_VMP_SDK_ROOT must be a fully qualified path.'
}
if (-not (Test-Path -LiteralPath $SdkRoot -PathType Container)) {
    throw "VMProtect SDK root does not exist: $SdkRoot"
}

$resolvedRoot = (Resolve-Path -LiteralPath $SdkRoot).ProviderPath
Assert-NoReparseAncestors -Path $resolvedRoot
$headerPath = Resolve-RequiredLeaf -Path (Join-Path $resolvedRoot 'Include\C\VMProtectSDK.h') -Description 'VMProtect C header'
$libraryPath = Resolve-RequiredLeaf -Path (Join-Path $resolvedRoot 'Lib\Windows\VMProtectSDK64.lib') -Description 'VMProtect x64 import library'
$dllPath = Resolve-RequiredLeaf -Path (Join-Path $resolvedRoot 'Lib\Windows\VMProtectSDK64.dll') -Description 'VMProtect x64 SDK DLL'

$headerHash = (Get-FileHash -LiteralPath $headerPath -Algorithm SHA256).Hash.ToUpperInvariant()
$libraryHash = (Get-FileHash -LiteralPath $libraryPath -Algorithm SHA256).Hash.ToUpperInvariant()
$dllHash = (Get-FileHash -LiteralPath $dllPath -Algorithm SHA256).Hash.ToUpperInvariant()
if ($headerHash -ne $ExpectedHeaderSha256 -or $libraryHash -ne $ExpectedImportLibrarySha256 -or
    $dllHash -ne $ExpectedSdkDllSha256) {
    throw 'VMProtect SDK identity hash mismatch; expected Lite v3.10.4 Build 2668 x64 SDK files.'
}

$normalizedHeader = ((Get-Content -LiteralPath $headerPath -Raw) -replace '\s+', ' ').Trim()
foreach ($declaration in $RequiredDeclarations) {
    if (-not $normalizedHeader.Contains($declaration)) {
        throw "VMProtect header is missing required declaration '$declaration': $headerPath"
    }
}

Assert-Amd64SdkImportArchive -Path $libraryPath
Assert-Amd64PeImage -Path $dllPath
$dumpbin = Resolve-X64Dumpbin
Assert-RequiredDllExports -Dumpbin $dumpbin -DllPath $dllPath

$result = [ordered]@{
    schema = 1
    verified = $true
    machine = 'AMD64'
    sdk_dll_identity = $ExpectedSdkDll
    required_symbols = @($RequiredSymbols)
    required_symbol_count = $RequiredSymbols.Count
    header_sha256 = $headerHash
    import_library_sha256 = $libraryHash
    sdk_dll_sha256 = $dllHash
    files_copied = 0
}
if ($AsJson) {
    $result | ConvertTo-Json -Depth 5 -Compress
}
else {
    Write-Output "VMProtect SDK root: $resolvedRoot"
    Write-Output "Header: $headerPath"
    Write-Output "Import library: $libraryPath (AMD64 COFF; $ExpectedSdkDll; 8 required symbols)"
    Write-Output "SDK DLL: $dllPath (AMD64 PE)"
    Write-Output "Required DLL exports: verified (8 symbols via $dumpbin)"
    Write-Output 'VMProtect SDK validation passed. No files were copied or modified.'
}
