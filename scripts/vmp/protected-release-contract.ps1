#requires -Version 7.4
#requires -PSEdition Core

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:NwflashRepositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).ProviderPath

function Get-NwflashProtectedMarkers {
    @(
        [pscustomobject]@{ symbol = 'nwflash_protection_accept_login_lease'; name = 'NWFlash.LoginLeaseAcceptance'; mode = 'Ultra' }
        [pscustomobject]@{ symbol = 'nwflash_protection_classify_heartbeat_lease'; name = 'NWFlash.HeartbeatLeaseClassification'; mode = 'Virtualization' }
        [pscustomobject]@{ symbol = 'nwflash_protection_admit_local_operation'; name = 'NWFlash.OperationAdmission'; mode = 'Ultra' }
        [pscustomobject]@{ symbol = 'nwflash_protection_verify_image_integrity'; name = 'NWFlash.ImageIntegrityDispatch'; mode = 'Virtualization' }
        [pscustomobject]@{ symbol = 'nwflash_protection_build_identity_matches'; name = 'NWFlash.BuildIdentity'; mode = 'Mutation' }
    )
}

function Get-NwflashRequiredSdkImports {
    @(
        'VMProtectBeginVirtualization'
        'VMProtectBeginMutation'
        'VMProtectBeginUltra'
        'VMProtectEnd'
        'VMProtectIsProtected'
        'VMProtectIsDebuggerPresent'
        'VMProtectIsVirtualMachinePresent'
        'VMProtectIsValidImageCRC'
    )
}

function Assert-PinnedSdkEvidence {
    param([Parameter(Mandatory)][object]$Evidence)

    if ($Evidence.schema -ne 1 -or -not [bool]$Evidence.verified -or
        [string]$Evidence.machine -ne 'AMD64' -or
        [string]$Evidence.sdk_dll_identity -ne 'VMProtectSDK64.dll' -or
        [int]$Evidence.required_symbol_count -ne 8 -or [int]$Evidence.files_copied -ne 0 -or
        [string]$Evidence.header_sha256 -ne '2300B7B4BB6BBF9CFA08013EC2D9B2FDCEB3DFD2E603CD1E24A493DE4D165B15' -or
        [string]$Evidence.import_library_sha256 -ne '9997A9C6E179010450385832A66EA36938E180FC9067D91FD6AAE7C9F6BF4D18' -or
        [string]$Evidence.sdk_dll_sha256 -ne 'EC3235136A4DAEE2A6F72C0F2994A8365CA8427C8068D068130B74C9FA64CD02') {
        throw 'SDK evidence does not match the pinned Lite v3.10.4 Build 2668 identity.'
    }
    $required = @(Get-NwflashRequiredSdkImports)
    $actual = @($Evidence.required_symbols)
    if ($actual.Count -ne $required.Count -or @($required | Where-Object { $actual -notcontains $_ }).Count -ne 0) {
        throw 'SDK evidence does not contain the exact required symbol set.'
    }
}

function Assert-LinkLayoutEvidence {
    param([Parameter(Mandatory)][object]$Evidence)

    if ($Evidence.schema -ne 1 -or -not [bool]$Evidence.verified -or $null -eq $Evidence.link_layout) {
        throw 'Link-layout evidence envelope is invalid.'
    }
    Assert-PinnedSdkEvidence -Evidence $Evidence.sdk
    $layout = $Evidence.link_layout
    if (-not [bool]$layout.verified -or [string]$layout.machine -ne 'AMD64' -or
        [string]$layout.imported_dll -ne 'VMProtectSDK64.dll' -or [int]$layout.files_copied -ne 0) {
        throw 'Link-layout evidence identity is invalid.'
    }
    $requiredImports = @(Get-NwflashRequiredSdkImports)
    $actualImports = @($layout.required_imports)
    if ($actualImports.Count -ne $requiredImports.Count -or
        @($requiredImports | Where-Object { $actualImports -notcontains $_ }).Count -ne 0) {
        throw 'Link-layout evidence does not contain the exact SDK import set.'
    }
    $expectedMarkers = @(Get-NwflashProtectedMarkers)
    $actualMarkers = @($layout.markers)
    if ($actualMarkers.Count -ne $expectedMarkers.Count) { throw 'Link-layout evidence has the wrong marker count.' }
    foreach ($expected in $expectedMarkers) {
        $actual = @($actualMarkers | Where-Object { [string]$_.symbol -eq [string]$expected.symbol })
        if ($actual.Count -ne 1 -or [string]$actual[0].mode -ne [string]$expected.mode -or
            [int]$actual[0].begin_count -ne 1 -or [int]$actual[0].end_count -ne 1 -or
            -not [bool]$actual[0].verified) {
            throw "Link-layout marker evidence is invalid for $($expected.symbol)."
        }
    }
}

function Get-NwflashRequiredProtectionOptions {
    [ordered]@{
        memory_protection = $true
        import_protection = $true
        packing = $true
        vm_execution_denial = $false
    }
}

function Get-NormalizedFullPath {
    param([Parameter(Mandatory)][string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path) -or -not [IO.Path]::IsPathFullyQualified($Path)) {
        throw "Path must be fully qualified: $Path"
    }
    [IO.Path]::GetFullPath($Path)
}

function Assert-PathNotReparsePoint {
    param([Parameter(Mandatory)][string]$Path)

    $fullPath = Get-NormalizedFullPath $Path
    if (-not (Test-Path -LiteralPath $fullPath)) {
        throw "Path is missing: $fullPath"
    }
    $item = Get-Item -LiteralPath $fullPath -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        [string]$item.LinkType -in @('SymbolicLink', 'Junction')) {
        throw "Reparse points are not allowed: $fullPath"
    }
    $fullPath
}

function Assert-NoReparseAncestors {
    param([Parameter(Mandatory)][string]$Path)

    $cursor = Get-NormalizedFullPath $Path
    while (-not (Test-Path -LiteralPath $cursor)) {
        $parent = Split-Path -Parent $cursor
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $cursor) {
            break
        }
        $cursor = $parent
    }
    while (-not [string]::IsNullOrWhiteSpace($cursor)) {
        if (Test-Path -LiteralPath $cursor) {
            Assert-PathNotReparsePoint $cursor | Out-Null
        }
        $parent = Split-Path -Parent $cursor
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $cursor) {
            break
        }
        $cursor = $parent
    }
}

function Resolve-FullyQualifiedLeaf {
    param([Parameter(Mandatory)][string]$Path, [switch]$AllowEmpty)

    $fullPath = Get-NormalizedFullPath $Path
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "Required file is missing: $fullPath"
    }
    Assert-NoReparseAncestors $fullPath
    $item = Get-Item -LiteralPath $fullPath -Force
    if (-not $AllowEmpty -and $item.Length -le 0) {
        throw "Required file is empty: $fullPath"
    }
    $item.FullName
}

function Resolve-SingleProtectedDesktopPdb {
    param([Parameter(Mandatory)][string]$ReleaseDirectory)

    $root = Get-NormalizedFullPath $ReleaseDirectory
    Assert-NoReparseAncestors $root
    $candidates = @(
        foreach ($name in @('nwflash-desktop.pdb', 'nwflash_desktop.pdb')) {
            $candidate = Join-Path $root $name
            if (Test-Path -LiteralPath $candidate -PathType Leaf) { $candidate }
        }
    )
    if ($candidates.Count -ne 1) {
        throw "Expected exactly one protected desktop PDB under $root."
    }
    Resolve-FullyQualifiedLeaf $candidates[0]
}

function Get-ReparseSafeTreeEntries {
    param([Parameter(Mandatory)][string]$Root)

    $rootPath = Get-NormalizedFullPath $Root
    if (-not (Test-Path -LiteralPath $rootPath -PathType Container)) {
        throw "Tree root is missing: $rootPath"
    }
    Assert-NoReparseAncestors $rootPath
    $entries = [Collections.Generic.List[object]]::new()
    $pending = [Collections.Generic.Stack[string]]::new()
    $pending.Push($rootPath)
    while ($pending.Count -ne 0) {
        $directory = $pending.Pop()
        foreach ($entry in Get-ChildItem -LiteralPath $directory -Force) {
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
                [string]$entry.LinkType -in @('SymbolicLink', 'Junction')) {
                throw "Reparse points are not allowed in a validated tree: $($entry.FullName)"
            }
            $entries.Add($entry)
            if ($entry.PSIsContainer) { $pending.Push($entry.FullName) }
        }
    }
    $entries.ToArray()
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)][string]$Path)
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

function Get-BytesSha256Hex {
    param([Parameter(Mandatory)][byte[]]$Bytes)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        [Convert]::ToHexString($algorithm.ComputeHash($Bytes))
    }
    finally {
        $algorithm.Dispose()
    }
}

function Assert-Amd64Pe {
    param([Parameter(Mandatory)][string]$Path)

    $leaf = Resolve-FullyQualifiedLeaf $Path
    $stream = [IO.File]::Open($leaf, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($stream.Length -lt 0x88) { throw "File is not a valid PE image: $leaf" }
        $reader = [IO.BinaryReader]::new($stream)
        try {
            if ($reader.ReadUInt16() -ne 0x5A4D) { throw "File is not a valid PE image: $leaf" }
            $stream.Position = 0x3C
            $peOffset = $reader.ReadInt32()
            if ($peOffset -lt 0 -or ($peOffset + 6) -gt $stream.Length) { throw "File has an invalid PE header offset: $leaf" }
            $stream.Position = $peOffset
            if ($reader.ReadUInt32() -ne 0x00004550) { throw "File is not a valid PE image: $leaf" }
            $machine = $reader.ReadUInt16()
            if ($machine -ne 0x8664) { throw "PE image is not AMD64 (0x8664): $leaf" }
        }
        finally {
            $reader.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
    'AMD64'
}

function Resolve-X64Dumpbin {
    if (-not [string]::IsNullOrWhiteSpace($env:NWFLASH_DUMPBIN_PATH)) {
        $candidate = Resolve-FullyQualifiedLeaf $env:NWFLASH_DUMPBIN_PATH
        if ([IO.Path]::GetFileName($candidate) -ine 'dumpbin.exe' -or $candidate -notmatch '(?i)\\Hostx64\\x64\\') {
            throw 'NWFLASH_DUMPBIN_PATH must identify x64 dumpbin.exe under Hostx64\x64.'
        }
        return $candidate
    }

    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw 'x64 dumpbin.exe is unavailable; set NWFLASH_DUMPBIN_PATH.'
    }
    $candidates = @(& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe')
    if ($LASTEXITCODE -ne 0) { throw 'vswhere failed while locating x64 dumpbin.exe.' }
    $candidate = @($candidates | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1)
    if ($candidate.Count -ne 1) { throw 'x64 dumpbin.exe is unavailable; install MSVC x64 tools or set NWFLASH_DUMPBIN_PATH.' }
    Resolve-FullyQualifiedLeaf $candidate[0]
}

function Get-PeCodeViewIdentity {
    param([Parameter(Mandatory)][string]$Executable)

    $path = Resolve-FullyQualifiedLeaf $Executable
    $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 0x100) { throw "PE image is too small to contain CodeView identity: $path" }
        if ($reader.ReadUInt16() -ne 0x5A4D) { throw "File is not a valid PE image: $path" }
        $stream.Position = 0x3C
        $peOffset = $reader.ReadInt32()
        if ($peOffset -lt 0 -or ($peOffset + 24) -gt $stream.Length) { throw "PE header offset is invalid: $path" }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw "File is not a valid PE image: $path" }
        $machine = $reader.ReadUInt16()
        $sectionCount = $reader.ReadUInt16()
        $stream.Position = $peOffset + 20
        $optionalHeaderSize = $reader.ReadUInt16()
        $optionalHeader = $peOffset + 24
        $stream.Position = $optionalHeader
        $magic = $reader.ReadUInt16()
        $dataDirectoryOffset = switch ($magic) {
            0x20B { $optionalHeader + 112 }
            0x10B { $optionalHeader + 96 }
            default { throw "PE optional-header magic is invalid: $path" }
        }
        $stream.Position = $dataDirectoryOffset + (6 * 8)
        $debugRva = $reader.ReadUInt32()
        $debugSize = $reader.ReadUInt32()
        if ($debugRva -eq 0 -or $debugSize -lt 28) { throw "PE image has no CodeView debug directory: $path" }

        $sectionTable = $optionalHeader + $optionalHeaderSize
        $debugOffset = $null
        for ($index = 0; $index -lt $sectionCount; $index++) {
            $section = $sectionTable + ($index * 40)
            if (($section + 40) -gt $stream.Length) { throw "PE section table is truncated: $path" }
            $stream.Position = $section + 8
            $virtualSize = $reader.ReadUInt32()
            $virtualAddress = $reader.ReadUInt32()
            $rawSize = $reader.ReadUInt32()
            $rawPointer = $reader.ReadUInt32()
            $extent = [Math]::Max([uint64]$virtualSize, [uint64]$rawSize)
            if ([uint64]$debugRva -ge [uint64]$virtualAddress -and [uint64]$debugRva -lt ([uint64]$virtualAddress + $extent)) {
                $debugOffset = [uint64]$rawPointer + ([uint64]$debugRva - [uint64]$virtualAddress)
                break
            }
        }
        if ($null -eq $debugOffset) { throw "PE debug directory RVA is not mapped by a section: $path" }

        for ($entry = 0; $entry -lt [Math]::Floor($debugSize / 28); $entry++) {
            $entryOffset = [uint64]$debugOffset + ([uint64]$entry * 28)
            if (($entryOffset + 28) -gt [uint64]$stream.Length) { throw "PE debug directory is truncated: $path" }
            $stream.Position = [int64]($entryOffset + 12)
            $type = $reader.ReadUInt32()
            $sizeOfData = $reader.ReadUInt32()
            $reader.ReadUInt32() | Out-Null
            $pointerToRawData = $reader.ReadUInt32()
            if ($type -ne 2 -or $sizeOfData -lt 24) { continue }
            if (([uint64]$pointerToRawData + $sizeOfData) -gt [uint64]$stream.Length) { throw "PE CodeView record is truncated: $path" }
            $stream.Position = $pointerToRawData
            if ($reader.ReadUInt32() -ne 0x53445352) { continue }
            $guidBytes = $reader.ReadBytes(16)
            $age = $reader.ReadUInt32()
            return [pscustomobject]@{
                machine = ('0x{0:X4}' -f $machine)
                guid = ([Guid]::new($guidBytes)).ToString('D').ToUpperInvariant()
                age = [uint32]$age
            }
        }
        throw "PE image has no RSDS CodeView identity: $path"
    }
    finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Get-PdbIdentity {
    param([Parameter(Mandatory)][string]$Pdb)

    $path = Resolve-FullyQualifiedLeaf $Pdb
    $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 56) { throw "PDB is too small to contain an MSF superblock: $path" }
        $signature = [Text.Encoding]::ASCII.GetString($reader.ReadBytes(32))
        if (-not $signature.StartsWith('Microsoft C/C++ MSF 7.00', [StringComparison]::Ordinal)) { throw "PDB is not MSF 7.00: $path" }
        $pageSize = $reader.ReadUInt32()
        $reader.ReadUInt32() | Out-Null
        $pageCount = $reader.ReadUInt32()
        $directorySize = $reader.ReadUInt32()
        $reader.ReadUInt32() | Out-Null
        $blockMapPage = $reader.ReadUInt32()
        if ($pageSize -lt 512 -or $pageSize -gt 65536 -or ($pageSize -band ($pageSize - 1)) -ne 0) { throw "PDB page size is invalid: $path" }
        if ([uint64]$pageCount * $pageSize -gt [uint64]$stream.Length + $pageSize) { throw "PDB page table exceeds the file: $path" }
        $directoryBlockCount = [int][Math]::Ceiling([double]$directorySize / $pageSize)
        if ($directoryBlockCount -le 0 -or $directoryBlockCount -gt ($pageSize / 4)) { throw "PDB directory block map is unsupported or invalid: $path" }
        $stream.Position = [int64]([uint64]$blockMapPage * $pageSize)
        $directoryBlocks = @(
            for ($index = 0; $index -lt $directoryBlockCount; $index++) { $reader.ReadUInt32() }
        )
        $directory = [byte[]]::new($directorySize)
        $written = 0
        foreach ($block in $directoryBlocks) {
            if ($block -ge $pageCount) { throw "PDB directory references an invalid page: $path" }
            $stream.Position = [int64]([uint64]$block * $pageSize)
            $count = [Math]::Min([int]$pageSize, [int]$directorySize - $written)
            $chunk = $reader.ReadBytes($count)
            if ($chunk.Length -ne $count) { throw "PDB directory page is truncated: $path" }
            [Array]::Copy($chunk, 0, $directory, $written, $count)
            $written += $count
        }

        $directoryStream = [IO.MemoryStream]::new($directory, $false)
        $directoryReader = [IO.BinaryReader]::new($directoryStream)
        try {
            $streamCount = $directoryReader.ReadUInt32()
            if ($streamCount -lt 2 -or $streamCount -gt 100000) { throw "PDB stream count is invalid: $path" }
            $streamSizes = @(
                for ($index = 0; $index -lt $streamCount; $index++) { $directoryReader.ReadUInt32() }
            )
            $streamOneBlocks = @()
            for ($streamIndex = 0; $streamIndex -le 1; $streamIndex++) {
                $size = $streamSizes[$streamIndex]
                $blockCount = if ($size -eq 0xFFFFFFFF) { 0 } else { [int][Math]::Ceiling([double]$size / $pageSize) }
                $blocks = @(
                    for ($blockIndex = 0; $blockIndex -lt $blockCount; $blockIndex++) { $directoryReader.ReadUInt32() }
                )
                if ($streamIndex -eq 1) { $streamOneBlocks = $blocks }
            }
            $streamOneSize = $streamSizes[1]
            if ($streamOneSize -lt 28 -or $streamOneSize -eq 0xFFFFFFFF) { throw "PDB identity stream is missing: $path" }
            $streamOne = [byte[]]::new($streamOneSize)
            $written = 0
            foreach ($block in $streamOneBlocks) {
                if ($block -ge $pageCount) { throw "PDB identity stream references an invalid page: $path" }
                $stream.Position = [int64]([uint64]$block * $pageSize)
                $count = [Math]::Min([int]$pageSize, [int]$streamOneSize - $written)
                $chunk = $reader.ReadBytes($count)
                if ($chunk.Length -ne $count) { throw "PDB identity stream is truncated: $path" }
                [Array]::Copy($chunk, 0, $streamOne, $written, $count)
                $written += $count
            }
            $age = [BitConverter]::ToUInt32($streamOne, 8)
            $guidBytes = [byte[]]::new(16)
            [Array]::Copy($streamOne, 12, $guidBytes, 0, 16)
            [pscustomobject]@{
                guid = ([Guid]::new($guidBytes)).ToString('D').ToUpperInvariant()
                age = [uint32]$age
            }
        }
        finally {
            $directoryReader.Dispose()
            $directoryStream.Dispose()
        }
    }
    finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Assert-MatchingPdb {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string]$Pdb
    )
    $exePath = Resolve-FullyQualifiedLeaf $Executable
    $pdbPath = Resolve-FullyQualifiedLeaf $Pdb
    $codeView = Get-PeCodeViewIdentity -Executable $exePath
    $pdbIdentity = Get-PdbIdentity -Pdb $pdbPath
    if ([string]$codeView.guid -ne [string]$pdbIdentity.guid -or [uint32]$codeView.age -ne [uint32]$pdbIdentity.age) {
        throw "PDB GUID/age does not match the exact executable: $pdbPath"
    }
}

function Assert-NoVmProtectImports {
    param([Parameter(Mandatory)][string]$Path, [string]$DumpbinPath)
    $leaf = Resolve-FullyQualifiedLeaf $Path
    if ([string]::IsNullOrWhiteSpace($DumpbinPath)) { $DumpbinPath = Resolve-X64Dumpbin }
    $output = (& $DumpbinPath /IMPORTS $leaf 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw "dumpbin import verification failed with exit code $LASTEXITCODE." }
    if ($output -match '(?i)VMProtect(?:SDK64\.dll|Begin|End|IsProtected|IsValidImageCRC|IsDebuggerPresent|IsVirtualMachinePresent)') {
        throw 'Protected output still imports VMProtect SDK symbols or VMProtectSDK64.dll.'
    }
}

function Assert-ExactVmProtectImports {
    param([Parameter(Mandatory)][string]$Path, [string]$DumpbinPath)

    $leaf = Resolve-FullyQualifiedLeaf $Path
    if ([string]::IsNullOrWhiteSpace($DumpbinPath)) { $DumpbinPath = Resolve-X64Dumpbin }
    $lines = @(& $DumpbinPath /NOLOGO /IMPORTS $leaf 2>&1)
    if (-not $?) { throw "dumpbin failed to inspect desktop VMProtect imports: $($lines -join [Environment]::NewLine)" }

    $expectedDll = 'VMProtectSDK64.dll'
    $dllIndexes = @(for ($index = 0; $index -lt $lines.Count; $index++) {
        if ($lines[$index].Trim().Equals($expectedDll, [StringComparison]::OrdinalIgnoreCase)) {
            $index
        }
    })
    if ($dllIndexes.Count -ne 1) {
        throw "Desktop input must contain exactly one $expectedDll import block; found $($dllIndexes.Count)."
    }

    $start = $dllIndexes[0]
    $end = $lines.Count
    for ($index = $start + 1; $index -lt $lines.Count; $index++) {
        if ($lines[$index] -match '^\s{4}\S+\.dll\s*$') { $end = $index; break }
    }
    $block = ($lines[$start..($end - 1)] -join [Environment]::NewLine)
    $required = @(Get-NwflashRequiredSdkImports)
    $actual = @([regex]::Matches($block, '(?m)^\s+[0-9A-F]+\s+(VMProtect\S+)\s*$') |
        ForEach-Object { $_.Groups[1].Value })
    foreach ($symbol in $required) {
        if ($actual -notcontains $symbol) { throw "Desktop input is missing required VMProtect import $symbol." }
    }
    foreach ($symbol in $actual) {
        if ($required -notcontains $symbol) { throw "Desktop input imports unexpected VMProtect symbol $symbol." }
    }
    if ($actual.Count -ne $required.Count) {
        throw "Desktop input VMProtect import count is $($actual.Count); expected $($required.Count)."
    }

    [pscustomobject]@{ verified = $true; imported_dll = $expectedDll; required_imports = $required }
}

function Assert-DesktopMarkerLayout {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string]$MapPath,
        [string]$DumpbinPath
    )
    $exePath = Resolve-FullyQualifiedLeaf $Executable
    $mapLeaf = Resolve-FullyQualifiedLeaf $MapPath
    if ([string]::IsNullOrWhiteSpace($DumpbinPath)) { $DumpbinPath = Resolve-X64Dumpbin }
    $mapText = Get-Content -Raw -LiteralPath $mapLeaf
    $disassembly = (& $DumpbinPath /DISASM:NOBYTES $exePath 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw "dumpbin disassembly failed with exit code $LASTEXITCODE." }

    foreach ($marker in Get-NwflashProtectedMarkers) {
        $symbol = [regex]::Escape($marker.symbol)
        $mapCount = [regex]::Matches($mapText, "(?m)^\s+[0-9A-Fa-f]{4}:[0-9A-Fa-f]{8}\s+$symbol\s+").Count
        if ($mapCount -ne 1) { throw "Desktop MAP must contain exactly one $($marker.symbol); found $mapCount." }
        $start = [regex]::Match($disassembly, "(?m)^\s*${symbol}:\s*$")
        if (-not $start.Success) { throw "Disassembly is missing marker region $($marker.symbol)." }
        $tail = $disassembly.Substring($start.Index + $start.Length)
        $next = [regex]::Match($tail, '(?m)^\S[^\r\n]*:\s*$')
        $region = if ($next.Success) { $tail.Substring(0, $next.Index) } else { $tail }
        $begin = [regex]::Matches($region, '(?im)\bcall\s+(?:qword ptr \[__imp_)?VMProtectBegin(Ultra|Virtualization|Mutation)\]?\s*$')
        $end = [regex]::Matches($region, '(?im)\bcall\s+(?:qword ptr \[__imp_)?VMProtectEnd\]?\s*$')
        if ($begin.Count -ne 1 -or $begin[0].Groups[1].Value -ne $marker.mode -or $end.Count -ne 1 -or $begin[0].Index -ge $end[0].Index) {
            throw "Marker region $($marker.symbol) does not contain exactly Begin$($marker.mode) followed by End."
        }
    }
    [pscustomobject]@{ verified = $true; marker_count = @(Get-NwflashProtectedMarkers).Count }
}

function Assert-ProtectedBuildEnvironment {
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_SESSION_VERIFY_KEY_B64)) {
        throw 'NWFLASH_SESSION_VERIFY_KEY_B64 is required.'
    }
    $encoded = $env:NWFLASH_SESSION_VERIFY_KEY_B64.Trim()
    if ($encoded -notmatch '^[A-Za-z0-9+/]+={0,2}$' -or ($encoded.Length % 4) -eq 1) {
        throw 'NWFLASH_SESSION_VERIFY_KEY_B64 must be standard Base64.'
    }
    $padded = $encoded.PadRight($encoded.Length + ((4 - ($encoded.Length % 4)) % 4), '=')
    try { $key = [Convert]::FromBase64String($padded) } catch { throw 'NWFLASH_SESSION_VERIFY_KEY_B64 must be standard Base64.' }
    if ($key.Length -ne 32) { throw 'NWFLASH_SESSION_VERIFY_KEY_B64 must decode to exactly 32 bytes.' }
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_BUILD_ID) -or $env:NWFLASH_BUILD_ID -notmatch '^[A-Za-z0-9._:-]{1,128}$') {
        throw 'NWFLASH_BUILD_ID must match [A-Za-z0-9._:-]{1,128}.'
    }
    if ([string]::IsNullOrWhiteSpace($env:NWFLASH_VMP_SDK_ROOT) -or -not [IO.Path]::IsPathFullyQualified($env:NWFLASH_VMP_SDK_ROOT)) {
        throw 'NWFLASH_VMP_SDK_ROOT must be a fully qualified external package root.'
    }
    $sdkRoot = Get-NormalizedFullPath $env:NWFLASH_VMP_SDK_ROOT
    if (-not (Test-Path -LiteralPath $sdkRoot -PathType Container)) { throw "NWFLASH_VMP_SDK_ROOT is missing: $sdkRoot" }
    Assert-PathNotReparsePoint $sdkRoot | Out-Null
    [pscustomobject]@{
        build_id = $env:NWFLASH_BUILD_ID
        verification_key_sha256 = Get-BytesSha256Hex $key
        sdk_root = $sdkRoot
    }
}

function Write-AtomicEvidence {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][object]$Value)
    $fullPath = Get-NormalizedFullPath $Path
    Assert-NoReparseAncestors $fullPath
    if (Test-Path -LiteralPath $fullPath) { throw "Evidence already exists and will not be overwritten: $fullPath" }
    $directory = Split-Path -Parent $fullPath
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) { throw "Evidence directory is missing: $directory" }
    Assert-NoReparseAncestors $directory
    $temporary = Join-Path $directory ('.' + [IO.Path]::GetFileName($fullPath) + '.tmp-' + [Guid]::NewGuid().ToString('N'))
    $json = $Value | ConvertTo-Json -Depth 20
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json + [Environment]::NewLine)
    $stream = [IO.FileStream]::new($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    }
    finally { $stream.Dispose() }
    try { [IO.File]::Move($temporary, $fullPath) } finally { if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary } }
    Assert-NoReparseAncestors $fullPath
    (Get-Item -LiteralPath $fullPath).IsReadOnly = $true
    $fullPath
}

function Read-ImmutableJsonEvidence {
    param([Parameter(Mandatory)][string]$Path)

    $leaf = Resolve-FullyQualifiedLeaf $Path
    if (-not (Get-Item -LiteralPath $leaf).IsReadOnly) { throw "Evidence is not immutable/read-only: $leaf" }
    try { $document = Get-Content -Raw -LiteralPath $leaf | ConvertFrom-Json } catch { throw "Evidence is not valid JSON: $leaf" }
    if ($document.schema -ne 1) { throw "Evidence schema is unsupported: $leaf" }
    $document
}

function Read-ProtectedEvidence {
    param([Parameter(Mandatory)][string]$Path, [string]$ExpectedState)

    $leaf = Resolve-FullyQualifiedLeaf $Path
    $document = Read-ImmutableJsonEvidence -Path $leaf
    $id = [Guid]::Empty
    if (-not [Guid]::TryParse([string]$document.handoff_id, [ref]$id) -or $id -eq [Guid]::Empty) { throw "Evidence handoff ID is invalid: $leaf" }
    if (-not [string]::IsNullOrWhiteSpace($ExpectedState) -and $document.state -ne $ExpectedState) {
        throw "Evidence state must be $ExpectedState, found $($document.state)."
    }
    $document
}

function New-DefaultProtectionOperations {
    $repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).ProviderPath
    [pscustomobject]@{
        CopyFile = { param($Source, $Destination) Copy-Item -LiteralPath $Source -Destination $Destination }
        GetSignature = { param($Path) Get-AuthenticodeSignature -LiteralPath $Path }
        AssertGitClean = {
            $status = @(& git -C $repoRoot status --porcelain=v1 --untracked-files=all)
            if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect the Git worktree before handoff.' }
            if ($status.Count -ne 0) { throw 'Protected handoff requires a clean Git worktree.' }
        }.GetNewClosure()
        GetGitCommit = {
            $commit = (& git -C $repoRoot rev-parse HEAD).Trim().ToLowerInvariant()
            if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[0-9a-f]{40}$') {
                throw 'Unable to record the exact Git commit for the handoff.'
            }
            $commit
        }.GetNewClosure()
        AssertMatchingPdb = { param($Exe, $Pdb) Assert-MatchingPdb -Executable $Exe -Pdb $Pdb }
        AssertMarkerLayout = { param($Exe, $Map) Assert-DesktopMarkerLayout -Executable $Exe -MapPath $Map }
        AssertExpectedVmProtectImports = { param($Path) Assert-ExactVmProtectImports -Path $Path }
        VerifySdk = {
            param($Sdk)
            $script = Join-Path $repoRoot 'scripts\vmp\verify-sdk.ps1'
            & $script -SdkRoot $Sdk -AsJson | ConvertFrom-Json
        }.GetNewClosure()
        VerifyLinkLayout = {
            param($Sdk)
            $script = Join-Path $repoRoot 'scripts\vmp\test-contracts.ps1'
            & $script -SdkRoot $Sdk -AsJson | ConvertFrom-Json
        }.GetNewClosure()
        AssertNoVmProtectImports = { param($Path) Assert-NoVmProtectImports -Path $Path }
        RunProbe = { param($Path, $Hash) Invoke-IsolatedProtectedProbe -Path $Path -ExpectedSha256 $Hash }
    }
}

function Invoke-PrepareManualHandoffCore {
    param(
        [Parameter(Mandatory)][string]$InputExe,
        [Parameter(Mandatory)][string]$InputPdb,
        [Parameter(Mandatory)][string]$InputMap,
        [Parameter(Mandatory)][string]$ProtectedOutputPath,
        [Parameter(Mandatory)][string]$CompilerLogPath,
        [Parameter(Mandatory)][string]$HandoffRoot,
        [Parameter(Mandatory)][object]$Operations
    )
    $environment = Assert-ProtectedBuildEnvironment
    $exe = Resolve-FullyQualifiedLeaf $InputExe
    $pdb = Resolve-FullyQualifiedLeaf $InputPdb
    $map = Resolve-FullyQualifiedLeaf $InputMap

    $output = Get-NormalizedFullPath $ProtectedOutputPath
    $log = Get-NormalizedFullPath $CompilerLogPath
    $handoffBase = Get-NormalizedFullPath $HandoffRoot
    Assert-NoReparseAncestors $output
    Assert-NoReparseAncestors $log
    Assert-NoReparseAncestors $handoffBase
    if ([IO.Path]::GetExtension($output) -ine '.exe') { throw 'Protected output path must end in .exe.' }
    $paths = @($exe, $pdb, $map, $output, $log)
    for ($i = 0; $i -lt $paths.Count; $i++) {
        for ($j = $i + 1; $j -lt $paths.Count; $j++) {
            if ($paths[$i].Equals($paths[$j], [StringComparison]::OrdinalIgnoreCase)) { throw 'All input, output, and compiler-log paths must be distinct.' }
        }
    }
    if (Test-Path -LiteralPath $output) { throw "Protected output already exists and will not be overwritten: $output" }
    if (Test-Path -LiteralPath $log) { throw "Compiler log already exists and will not be overwritten: $log" }
    if (-not (Test-Path -LiteralPath $handoffBase)) { New-Item -ItemType Directory -Path $handoffBase | Out-Null }
    Assert-PathNotReparsePoint $handoffBase | Out-Null

    $handoffId = [Guid]::NewGuid().ToString('D')
    $handoffDirectory = Join-Path $handoffBase $handoffId
    if (Test-Path -LiteralPath $handoffDirectory) { throw "Handoff directory already exists: $handoffDirectory" }
    $inputDirectory = Join-Path $handoffDirectory 'input'
    $evidenceDirectory = Join-Path $handoffDirectory 'evidence'
    New-Item -ItemType Directory -Path $inputDirectory | Out-Null
    New-Item -ItemType Directory -Path $evidenceDirectory | Out-Null
    $copiedExe = Join-Path $inputDirectory 'nwflash-desktop.exe'
    $copiedPdb = Join-Path $inputDirectory 'nwflash-desktop.pdb'
    $copiedMap = Join-Path $inputDirectory 'nwflash-desktop.map'
    & $Operations.CopyFile $exe $copiedExe
    & $Operations.CopyFile $pdb $copiedPdb
    & $Operations.CopyFile $map $copiedMap

    # The staged snapshot is the sole validation and evidence authority. Source
    # files are never read again after copying starts, so replacements cannot
    # transfer proof from one generation to another.
    $copiedExe = Resolve-FullyQualifiedLeaf $copiedExe
    $copiedPdb = Resolve-FullyQualifiedLeaf $copiedPdb
    $copiedMap = Resolve-FullyQualifiedLeaf $copiedMap
    Assert-Amd64Pe $copiedExe | Out-Null
    $signature = & $Operations.GetSignature $copiedExe
    if ([string]$signature.Status -ne 'NotSigned') { throw "Pre-VMP input must be unsigned; status was $($signature.Status)." }
    & $Operations.AssertMatchingPdb $copiedExe $copiedPdb
    $desktopImports = & $Operations.AssertExpectedVmProtectImports $copiedExe
    $markerLayout = & $Operations.AssertMarkerLayout $copiedExe $copiedMap
    $sdkResult = & $Operations.VerifySdk $environment.sdk_root
    $linkResult = & $Operations.VerifyLinkLayout $environment.sdk_root
    $sdkEvidence = Write-AtomicEvidence -Path (Join-Path $evidenceDirectory 'sdk-verification.json') -Value $sdkResult
    $linkEvidence = Write-AtomicEvidence -Path (Join-Path $evidenceDirectory 'link-layout.json') -Value $linkResult
    foreach ($path in @($copiedExe, $copiedPdb, $copiedMap)) { (Get-Item -LiteralPath $path).IsReadOnly = $true }

    & $Operations.AssertGitClean
    $gitCommit = & $Operations.GetGitCommit
    $prepared = [ordered]@{
        schema = 1
        handoff_id = $handoffId
        state = 'prepared'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = $null
        git_commit = $gitCommit
        build_id = $environment.build_id
        verification_key_sha256 = $environment.verification_key_sha256
        input_exe = [ordered]@{ path = $copiedExe; length = (Get-Item $copiedExe).Length; sha256 = Get-Sha256Hex $copiedExe; machine = 'AMD64'; authenticode = 'NotSigned' }
        input_pdb = [ordered]@{ path = $copiedPdb; length = (Get-Item $copiedPdb).Length; sha256 = Get-Sha256Hex $copiedPdb; matches_input_exe = $true }
        input_map = [ordered]@{ path = $copiedMap; length = (Get-Item $copiedMap).Length; sha256 = Get-Sha256Hex $copiedMap; marker_layout_verified = [bool]$markerLayout.verified }
        input_vmprotect_imports = [ordered]@{ verified = [bool]$desktopImports.verified; imported_dll = [string]$desktopImports.imported_dll; required_imports = @($desktopImports.required_imports) }
        sdk_verification_sha256 = Get-Sha256Hex $sdkEvidence
        link_layout_sha256 = Get-Sha256Hex $linkEvidence
        protected_output_path = $output
        compiler_log_path = $log
        markers = @(Get-NwflashProtectedMarkers)
        required_options = Get-NwflashRequiredProtectionOptions
    }
    Write-AtomicEvidence -Path (Join-Path $evidenceDirectory 'prepared.json') -Value $prepared
}

function Assert-MarkerReview {
    param(
        [Parameter(Mandatory)][object]$Review,
        [Parameter(Mandatory)][object]$Prepared,
        [Parameter(Mandatory)][string]$PreparedHash,
        [Parameter(Mandatory)][string]$CompilerLogHash,
        [Parameter(Mandatory)][string]$ProtectedOutputHash
    )
    if ($Review.schema -ne 1 -or [string]$Review.handoff_id -ne [string]$Prepared.handoff_id) { throw 'Marker review schema or handoff ID is invalid.' }
    if ([string]$Review.prepared_manifest_sha256 -ne $PreparedHash -or [string]$Review.compiler_log_sha256 -ne $CompilerLogHash) { throw 'Marker review hash binding is invalid.' }
    if (-not ([string]$Review.protected_output_sha256).Equals($ProtectedOutputHash, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Marker review is not bound to the exact protected output hash.'
    }
    if ([string]$Review.vmprotect_edition -ne 'Lite' -or [string]::IsNullOrWhiteSpace([string]$Review.vmprotect_version)) { throw 'Marker review must identify VMProtect Lite and its observed version.' }
    if (-not [bool]$Review.compiler_log_reviewed -or [bool]$Review.compiler_log_has_errors) { throw 'VMProtect compiler log review did not pass.' }
    if ([string]::IsNullOrWhiteSpace([string]$Review.operator)) { throw 'Marker review operator identity is required.' }
    $expectedOptions = Get-NwflashRequiredProtectionOptions
    foreach ($name in $expectedOptions.Keys) {
        if ([bool]$Review.options.$name -ne [bool]$expectedOptions[$name]) { throw "Marker review option is invalid: $name" }
    }
    $expectedMarkers = @(Get-NwflashProtectedMarkers)
    $actualMarkers = @($Review.markers)
    if ($actualMarkers.Count -ne $expectedMarkers.Count) { throw 'Marker review does not contain the exact marker set.' }
    $seen = @{}
    foreach ($marker in $actualMarkers) {
        if ($seen.ContainsKey([string]$marker.name)) { throw "Marker review contains a duplicate: $($marker.name)" }
        $seen[[string]$marker.name] = $true
        $expected = @($expectedMarkers | Where-Object name -eq ([string]$marker.name))
        if ($expected.Count -ne 1 -or [string]$marker.mode -ne [string]$expected[0].mode -or -not [bool]$marker.compiled) { throw "Marker review is invalid for $($marker.name)." }
    }
}

function Assert-ProtectedProbeResult {
    param(
        [Parameter(Mandatory)][object]$Result,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [string]$ExpectedBuildId
    )
    $exitCode = [int]$Result.exit_code
    if ($exitCode -ne 0) { throw "Protected runtime probe failed with exit code $exitCode." }
    if (-not [bool]$Result.probe_available -or -not [bool]$Result.VMProtectIsProtected -or -not [bool]$Result.VMProtectIsValidImageCRC) {
        throw 'Protected runtime probe did not return valid protection and CRC signals.'
    }
    if (-not ([string]$Result.observed_sha256).Equals($ExpectedSha256, [StringComparison]::OrdinalIgnoreCase)) { throw 'Protected runtime probe hash does not match the accepted output.' }
    if ([string]::IsNullOrWhiteSpace([string]$Result.build_id)) {
        throw 'Protected runtime probe did not report its compiled build identity.'
    }
    if (-not [string]::IsNullOrWhiteSpace($ExpectedBuildId) -and
        -not ([string]$Result.build_id).Equals($ExpectedBuildId, [StringComparison]::Ordinal)) {
        throw 'Protected runtime probe build identity does not match prepared evidence.'
    }
}

function Invoke-AcceptManualOutputCore {
    param(
        [Parameter(Mandatory)][string]$PreparedManifest,
        [Parameter(Mandatory)][string]$MarkerReviewPath,
        [Parameter(Mandatory)][object]$Operations
    )
    $preparedPath = Resolve-FullyQualifiedLeaf $PreparedManifest
    $prepared = Read-ProtectedEvidence -Path $preparedPath -ExpectedState 'prepared'
    $preparedHash = Get-Sha256Hex $preparedPath
    $evidenceDirectory = Split-Path -Parent $preparedPath
    $sdkEvidencePath = Resolve-FullyQualifiedLeaf (Join-Path $evidenceDirectory 'sdk-verification.json')
    $linkEvidencePath = Resolve-FullyQualifiedLeaf (Join-Path $evidenceDirectory 'link-layout.json')
    $sdkEvidence = Read-ImmutableJsonEvidence -Path $sdkEvidencePath
    $linkEvidence = Read-ImmutableJsonEvidence -Path $linkEvidencePath
    Assert-PinnedSdkEvidence -Evidence $sdkEvidence
    Assert-LinkLayoutEvidence -Evidence $linkEvidence
    if (-not [bool]$sdkEvidence.verified -or (Get-Sha256Hex $sdkEvidencePath) -ne [string]$prepared.sdk_verification_sha256) {
        throw 'SDK evidence is not the verified sidecar bound by prepared.json.'
    }
    if (-not [bool]$linkEvidence.verified -or (Get-Sha256Hex $linkEvidencePath) -ne [string]$prepared.link_layout_sha256) {
        throw 'Link-layout evidence is not the verified sidecar bound by prepared.json.'
    }
    $inputExe = Resolve-FullyQualifiedLeaf ([string]$prepared.input_exe.path)
    $inputPdb = Resolve-FullyQualifiedLeaf ([string]$prepared.input_pdb.path)
    $inputMap = Resolve-FullyQualifiedLeaf ([string]$prepared.input_map.path)
    foreach ($record in @($prepared.input_exe, $prepared.input_pdb, $prepared.input_map)) {
        $leaf = Resolve-FullyQualifiedLeaf ([string]$record.path)
        if ((Get-Item -LiteralPath $leaf).Length -ne [long]$record.length -or (Get-Sha256Hex $leaf) -ne [string]$record.sha256) { throw "Prepared input was modified: $leaf" }
    }
    Assert-Amd64Pe $inputExe | Out-Null
    $inputSignature = & $Operations.GetSignature $inputExe
    if ([string]$inputSignature.Status -ne 'NotSigned') { throw 'Prepared input EXE is no longer unsigned.' }
    & $Operations.AssertMatchingPdb $inputExe $inputPdb
    & $Operations.AssertExpectedVmProtectImports $inputExe | Out-Null
    & $Operations.AssertMarkerLayout $inputExe $inputMap | Out-Null

    $output = Resolve-FullyQualifiedLeaf ([string]$prepared.protected_output_path)
    if (-not $output.Equals((Get-NormalizedFullPath ([string]$prepared.protected_output_path)), [StringComparison]::OrdinalIgnoreCase)) { throw 'Protected output path does not match prepared evidence.' }
    foreach ($path in @($inputExe, $inputPdb, $inputMap, $preparedPath, [string]$prepared.compiler_log_path)) {
        if ($output.Equals((Get-NormalizedFullPath $path), [StringComparison]::OrdinalIgnoreCase)) { throw 'Protected output must be distinct from every input and evidence path.' }
    }
    $outputHash = Get-Sha256Hex $output
    if ($outputHash -eq [string]$prepared.input_exe.sha256) { throw 'Protected output SHA-256 must differ from the unprotected input.' }
    Assert-Amd64Pe $output | Out-Null
    $outputSignature = & $Operations.GetSignature $output
    if ([string]$outputSignature.Status -ne 'NotSigned') { throw 'Protected output must remain unsigned until acceptance completes.' }
    & $Operations.AssertNoVmProtectImports $output

    $compilerLog = Resolve-FullyQualifiedLeaf ([string]$prepared.compiler_log_path)
    $logText = Get-Content -Raw -LiteralPath $compilerLog
    if ($logText -match '(?im)\[Error\]|Compilation failed') { throw 'VMProtect compiler log contains a fatal error marker.' }
    $compilerHash = Get-Sha256Hex $compilerLog
    $reviewPath = Resolve-FullyQualifiedLeaf $MarkerReviewPath
    try { $review = Get-Content -Raw -LiteralPath $reviewPath | ConvertFrom-Json } catch { throw 'Marker review is not valid JSON.' }
    Assert-MarkerReview -Review $review -Prepared $prepared -PreparedHash $preparedHash `
        -CompilerLogHash $compilerHash -ProtectedOutputHash $outputHash
    $reviewHash = Get-Sha256Hex $reviewPath
    $probeResult = & $Operations.RunProbe $output $outputHash
    Assert-ProtectedProbeResult -Result $probeResult -ExpectedSha256 $outputHash -ExpectedBuildId ([string]$prepared.build_id)
    if ((Get-Sha256Hex $output) -ne $outputHash) { throw 'Protected output changed while acceptance was running.' }

    foreach ($path in @($output, $compilerLog, $reviewPath)) { (Get-Item -LiteralPath $path).IsReadOnly = $true }
    $accepted = [ordered]@{
        schema = 1
        handoff_id = [string]$prepared.handoff_id
        state = 'accepted'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = $preparedHash
        prepared_manifest_sha256 = $preparedHash
        prepared_manifest_path = $preparedPath
        sdk_verification_path = $sdkEvidencePath
        sdk_verification_sha256 = Get-Sha256Hex $sdkEvidencePath
        link_layout_path = $linkEvidencePath
        link_layout_sha256 = Get-Sha256Hex $linkEvidencePath
        git_commit = [string]$prepared.git_commit
        build_id = [string]$prepared.build_id
        verification_key_sha256 = [string]$prepared.verification_key_sha256
        input_exe_sha256 = [string]$prepared.input_exe.sha256
        input_pdb_sha256 = [string]$prepared.input_pdb.sha256
        input_map_sha256 = [string]$prepared.input_map.sha256
        protected_output = [ordered]@{ path = $output; length = (Get-Item $output).Length; sha256 = $outputHash; machine = 'AMD64'; authenticode = 'NotSigned' }
        sdk_imports_present = $false
        compiler_log_path = $compilerLog
        compiler_log_sha256 = $compilerHash
        marker_review_path = $reviewPath
        marker_review_sha256 = $reviewHash
        is_protected = $true
        image_crc_valid = $true
        runtime_probe_exit_code = 0
        protected_build_id = [string]$probeResult.build_id
    }
    Write-AtomicEvidence -Path (Join-Path (Split-Path -Parent $preparedPath) 'accepted.json') -Value $accepted
}

function Assert-AcceptedEvidenceChain {
    param(
        [Parameter(Mandatory)][string]$AcceptedEvidence,
        [Parameter(Mandatory)][object]$Operations
    )

    $acceptedPath = Resolve-FullyQualifiedLeaf $AcceptedEvidence
    $accepted = Read-ProtectedEvidence -Path $acceptedPath -ExpectedState 'accepted'
    $preparedPath = Resolve-FullyQualifiedLeaf ([string]$accepted.prepared_manifest_path)
    $preparedEvidenceDirectory = Split-Path -Parent $preparedPath
    $acceptedEvidenceDirectory = Split-Path -Parent $acceptedPath
    if (-not $preparedEvidenceDirectory.Equals($acceptedEvidenceDirectory, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Accepted and prepared evidence must reside in the same immutable evidence directory.'
    }
    $prepared = Read-ProtectedEvidence -Path $preparedPath -ExpectedState 'prepared'
    $preparedHash = Get-Sha256Hex $preparedPath
    if ([string]$accepted.handoff_id -ne [string]$prepared.handoff_id -or
        [string]$accepted.previous_evidence_sha256 -ne $preparedHash -or
        [string]$accepted.prepared_manifest_sha256 -ne $preparedHash) {
        throw 'Accepted evidence is not hash-bound to the exact prepared evidence.'
    }

    $sdkEvidencePath = Resolve-FullyQualifiedLeaf ([string]$accepted.sdk_verification_path)
    $linkEvidencePath = Resolve-FullyQualifiedLeaf ([string]$accepted.link_layout_path)
    $evidenceDirectory = $preparedEvidenceDirectory
    if (-not $sdkEvidencePath.Equals((Join-Path $evidenceDirectory 'sdk-verification.json'), [StringComparison]::OrdinalIgnoreCase) -or
        -not $linkEvidencePath.Equals((Join-Path $evidenceDirectory 'link-layout.json'), [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Accepted SDK/link evidence paths do not match the prepared evidence directory.'
    }
    $sdkEvidence = Read-ImmutableJsonEvidence -Path $sdkEvidencePath
    $linkEvidence = Read-ImmutableJsonEvidence -Path $linkEvidencePath
    Assert-PinnedSdkEvidence -Evidence $sdkEvidence
    Assert-LinkLayoutEvidence -Evidence $linkEvidence
    $sdkHash = Get-Sha256Hex $sdkEvidencePath
    $linkHash = Get-Sha256Hex $linkEvidencePath
    if (-not [bool]$sdkEvidence.verified -or $sdkHash -ne [string]$prepared.sdk_verification_sha256 -or
        $sdkHash -ne [string]$accepted.sdk_verification_sha256) {
        throw 'Accepted SDK evidence hash or verified state is invalid.'
    }
    if (-not [bool]$linkEvidence.verified -or $linkHash -ne [string]$prepared.link_layout_sha256 -or
        $linkHash -ne [string]$accepted.link_layout_sha256) {
        throw 'Accepted link-layout evidence hash or verified state is invalid.'
    }

    $inputExe = Resolve-FullyQualifiedLeaf ([string]$prepared.input_exe.path)
    $inputPdb = Resolve-FullyQualifiedLeaf ([string]$prepared.input_pdb.path)
    $inputMap = Resolve-FullyQualifiedLeaf ([string]$prepared.input_map.path)
    foreach ($record in @($prepared.input_exe, $prepared.input_pdb, $prepared.input_map)) {
        $leaf = Resolve-FullyQualifiedLeaf ([string]$record.path)
        if (-not (Get-Item -LiteralPath $leaf).IsReadOnly -or
            (Get-Item -LiteralPath $leaf).Length -ne [long]$record.length -or
            (Get-Sha256Hex $leaf) -ne [string]$record.sha256) {
            throw "Prepared input changed after acceptance: $leaf"
        }
    }
    if ([string]$accepted.input_exe_sha256 -ne [string]$prepared.input_exe.sha256 -or
        [string]$accepted.input_pdb_sha256 -ne [string]$prepared.input_pdb.sha256 -or
        [string]$accepted.input_map_sha256 -ne [string]$prepared.input_map.sha256) {
        throw 'Accepted input hashes do not match prepared evidence.'
    }
    Assert-Amd64Pe $inputExe | Out-Null
    $inputSignature = & $Operations.GetSignature $inputExe
    if ([string]$inputSignature.Status -ne 'NotSigned') {
        throw 'Prepared input EXE must remain unsigned.'
    }
    & $Operations.AssertMatchingPdb $inputExe $inputPdb
    $desktopImports = & $Operations.AssertExpectedVmProtectImports $inputExe
    if (-not [bool]$desktopImports.verified) { throw 'Prepared desktop VMProtect import contract failed.' }
    $requiredImports = @(Get-NwflashRequiredSdkImports)
    $recordedImports = @($prepared.input_vmprotect_imports.required_imports)
    if (-not [bool]$prepared.input_vmprotect_imports.verified -or
        [string]$prepared.input_vmprotect_imports.imported_dll -ne 'VMProtectSDK64.dll' -or
        $recordedImports.Count -ne $requiredImports.Count -or
        @($requiredImports | Where-Object { $recordedImports -notcontains $_ }).Count -ne 0) {
        throw 'Prepared desktop VMProtect import evidence is incomplete or inconsistent.'
    }
    & $Operations.AssertMarkerLayout $inputExe $inputMap | Out-Null

    $output = Resolve-FullyQualifiedLeaf ([string]$accepted.protected_output.path)
    if (-not $output.Equals((Get-NormalizedFullPath ([string]$prepared.protected_output_path)), [StringComparison]::OrdinalIgnoreCase) -or
        (Get-Item -LiteralPath $output).Length -ne [long]$accepted.protected_output.length -or
        (Get-Sha256Hex $output) -ne [string]$accepted.protected_output.sha256 -or
        (Get-Sha256Hex $output) -eq [string]$prepared.input_exe.sha256) {
        throw 'Accepted protected output path, length, or hash is invalid.'
    }
    if (-not (Get-Item -LiteralPath $output).IsReadOnly) { throw 'Accepted protected output is not immutable/read-only.' }
    Assert-Amd64Pe $output | Out-Null
    $outputSignature = & $Operations.GetSignature $output
    if ([string]$outputSignature.Status -ne 'NotSigned') {
        throw 'Accepted protected output must remain unsigned before the signing copy is created.'
    }
    & $Operations.AssertNoVmProtectImports $output
    $outputHash = Get-Sha256Hex $output

    $compilerLog = Resolve-FullyQualifiedLeaf ([string]$accepted.compiler_log_path)
    $reviewPath = Resolve-FullyQualifiedLeaf ([string]$accepted.marker_review_path)
    if (-not (Get-Item -LiteralPath $compilerLog).IsReadOnly -or
        -not (Get-Item -LiteralPath $reviewPath).IsReadOnly -or
        (Get-Sha256Hex $compilerLog) -ne [string]$accepted.compiler_log_sha256 -or
        (Get-Sha256Hex $reviewPath) -ne [string]$accepted.marker_review_sha256) {
        throw 'Accepted compiler log or marker review changed after acceptance.'
    }
    $logText = Get-Content -Raw -LiteralPath $compilerLog
    if ($logText -match '(?im)\[Error\]|Compilation failed') { throw 'VMProtect compiler log contains a fatal error marker.' }
    $review = Read-ImmutableJsonEvidence -Path $reviewPath
    Assert-MarkerReview -Review $review -Prepared $prepared -PreparedHash $preparedHash `
        -CompilerLogHash (Get-Sha256Hex $compilerLog) -ProtectedOutputHash $outputHash

    $probeResult = & $Operations.RunProbe $output $outputHash
    Assert-ProtectedProbeResult -Result $probeResult -ExpectedSha256 $outputHash -ExpectedBuildId ([string]$prepared.build_id)
    if ((Get-Sha256Hex $output) -ne $outputHash) { throw 'Protected output changed during final acceptance revalidation.' }
    if ([string]$accepted.git_commit -ne [string]$prepared.git_commit -or
        [string]$accepted.build_id -ne [string]$prepared.build_id -or
        [string]$accepted.protected_build_id -ne [string]$prepared.build_id -or
        [string]$accepted.verification_key_sha256 -ne [string]$prepared.verification_key_sha256 -or
        -not [bool]$accepted.is_protected -or -not [bool]$accepted.image_crc_valid -or
        [int]$accepted.runtime_probe_exit_code -ne 0 -or [bool]$accepted.sdk_imports_present) {
        throw 'Accepted evidence summary fields are inconsistent with the verified chain.'
    }

    [pscustomobject]@{
        accepted_path = $acceptedPath
        accepted = $accepted
        prepared_path = $preparedPath
        prepared = $prepared
        protected_output = $output
    }
}

function Assert-NsisBuiltEvidenceChain {
    param(
        [Parameter(Mandatory)][string]$NsisEvidence,
        [Parameter(Mandatory)][object]$Operations
    )

    $nsisPath = Resolve-FullyQualifiedLeaf $NsisEvidence
    $nsis = Read-ProtectedEvidence -Path $nsisPath -ExpectedState 'nsis-built'
    $evidenceDirectory = Split-Path -Parent $nsisPath
    $exeEvidencePath = Resolve-FullyQualifiedLeaf (Join-Path $evidenceDirectory 'exe-signed.json')
    $exeEvidence = Read-ProtectedEvidence -Path $exeEvidencePath -ExpectedState 'exe-signed'
    if ((Get-Sha256Hex $exeEvidencePath) -ne [string]$nsis.previous_evidence_sha256 -or
        [string]$exeEvidence.handoff_id -ne [string]$nsis.handoff_id -or
        [string]$exeEvidence.signed_sha256 -ne [string]$nsis.signed_exe_sha256) {
        throw 'NSIS evidence is not bound to the exact signed EXE evidence.'
    }

    $acceptedPath = Resolve-FullyQualifiedLeaf ([string]$exeEvidence.input_evidence_path)
    $acceptedChain = Assert-AcceptedEvidenceChain -AcceptedEvidence $acceptedPath -Operations $Operations
    $acceptedHash = Get-Sha256Hex $acceptedPath
    if ([string]$exeEvidence.previous_evidence_sha256 -ne $acceptedHash -or
        [string]$exeEvidence.input_evidence_sha256 -ne $acceptedHash -or
        [string]$exeEvidence.handoff_id -ne [string]$acceptedChain.accepted.handoff_id -or
        [string]$exeEvidence.unsigned_sha256 -ne [string]$acceptedChain.accepted.protected_output.sha256) {
        throw 'Signed EXE evidence is not bound to the full accepted handoff chain.'
    }
    $signedExe = Resolve-FullyQualifiedLeaf ([string]$exeEvidence.target_path)
    if ((Get-Sha256Hex $signedExe) -ne [string]$exeEvidence.signed_sha256) {
        throw 'Signed EXE changed before NSIS signing.'
    }

    $installer = Resolve-FullyQualifiedLeaf ([string]$nsis.installer_path)
    if ((Get-Sha256Hex $installer) -ne [string]$nsis.installer_unsigned_sha256) {
        throw 'Unsigned installer changed after nsis-built evidence was written.'
    }

    [pscustomobject]@{
        nsis_path = $nsisPath
        nsis = $nsis
        exe_evidence_path = $exeEvidencePath
        exe_evidence = $exeEvidence
        accepted_chain = $acceptedChain
        installer = $installer
    }
}

function Assert-InstallerSigningTarget {
    param(
        [Parameter(Mandatory)][string]$Target,
        [Parameter(Mandatory)][ValidatePattern('^[0-9A-Fa-f]{64}$')][string]$ExpectedUnsignedSha256,
        [Parameter(Mandatory)][string]$NsisEvidence,
        [Parameter(Mandatory)][object]$Operations
    )

    $targetPath = Resolve-FullyQualifiedLeaf $Target
    $chain = Assert-NsisBuiltEvidenceChain -NsisEvidence $NsisEvidence -Operations $Operations
    if (-not $targetPath.Equals([string]$chain.installer, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Installer signing target path does not match nsis-built evidence.'
    }
    $expected = $ExpectedUnsignedSha256.ToUpperInvariant()
    if ($expected -ne [string]$chain.nsis.installer_unsigned_sha256 -or
        (Get-Sha256Hex $targetPath) -ne $expected) {
        throw 'Installer signing hash does not match nsis-built evidence.'
    }
    $chain
}

function Remove-ValidatedTemporaryRoot {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Prefix)
    $fullPath = Get-NormalizedFullPath $Root
    $tempPath = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
    $parent = [IO.Path]::GetFullPath((Split-Path -Parent $fullPath)).TrimEnd('\', '/')
    $leaf = Split-Path -Leaf $fullPath
    if (-not $parent.Equals($tempPath, [StringComparison]::OrdinalIgnoreCase) -or $leaf -notmatch ('^' + [regex]::Escape($Prefix) + '[0-9a-f]{32}$')) {
        throw "Refusing recursive cleanup outside a validated temporary root: $fullPath"
    }
    if (Test-Path -LiteralPath $fullPath) {
        Assert-NoReparseAncestors $fullPath
        Get-ReparseSafeTreeEntries -Root $fullPath | Out-Null
        Remove-Item -LiteralPath $fullPath -Recurse -Force
    }
}

function Initialize-VerifiedInstallRoot {
    param([Parameter(Mandatory)][string]$Path)

    $root = Get-NormalizedFullPath $Path
    if (Test-Path -LiteralPath $root) {
        if (-not (Test-Path -LiteralPath $root -PathType Container)) {
            throw "Install root exists but is not a directory: $root"
        }
        Assert-NoReparseAncestors $root
        if (@(Get-ReparseSafeTreeEntries -Root $root).Count -ne 0) {
            throw "Install root must be fresh and empty: $root"
        }
    }
    else {
        Assert-NoReparseAncestors $root
        New-Item -ItemType Directory -Path $root | Out-Null
        Assert-NoReparseAncestors $root
    }
    $root
}

function Invoke-IsolatedProtectedProbe {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$ExpectedSha256,
        [ValidateRange(1000, 60000)][int]$TimeoutMilliseconds = 15000
    )
    $source = Resolve-FullyQualifiedLeaf $Path
    $root = Join-Path ([IO.Path]::GetTempPath()) ('nwflash-vmp-probe-' + [Guid]::NewGuid().ToString('N'))
    $priorPath = $env:PATH
    try {
        New-Item -ItemType Directory -Path $root | Out-Null
        $probeExe = Join-Path $root 'nwflash-desktop.protected.exe'
        $stdout = Join-Path $root 'stdout.jsonl'
        $stderr = Join-Path $root 'stderr.txt'
        Copy-Item -LiteralPath $source -Destination $probeExe
        if ((Get-Sha256Hex $probeExe) -ne $ExpectedSha256) { throw 'Isolated protected probe copy hash mismatch.' }
        $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
        try {
            $process = Start-Process -FilePath $probeExe -ArgumentList '--nwflash-protected-release-probe' -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
            if (-not $process.WaitForExit($TimeoutMilliseconds)) {
                try { $process.Kill($true) } catch {}
                $process.WaitForExit()
                throw "Protected runtime probe timed out after $TimeoutMilliseconds ms."
            }
            $process.WaitForExit()
        }
        catch { throw 'Protected runtime probe failed with exit code 44.' }
        $lines = @(Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($lines.Count -ne 1) { throw "Protected runtime probe failed with exit code 44: expected one JSON line." }
        try { $result = $lines[0] | ConvertFrom-Json } catch { throw 'Protected runtime probe failed with exit code 44: malformed JSON.' }
        $result | Add-Member -NotePropertyName exit_code -NotePropertyValue $process.ExitCode -Force
        $result | Add-Member -NotePropertyName observed_sha256 -NotePropertyValue (Get-Sha256Hex $probeExe) -Force
        $result
    }
    finally {
        $env:PATH = $priorPath
        Remove-ValidatedTemporaryRoot -Root $root -Prefix 'nwflash-vmp-probe-'
    }
}

function Assert-AuthenticodeIdentity {
    param([Parameter(Mandatory)][object]$Signature, [Parameter(Mandatory)][string]$ExpectedThumbprint)
    $thumbprint = ($ExpectedThumbprint -replace '\s', '').ToUpperInvariant()
    if ($thumbprint -notmatch '^[0-9A-F]{40}$') { throw 'Expected certificate thumbprint must be 40 hexadecimal characters.' }
    if ([string]$Signature.Status -ne 'Valid' -or $null -eq $Signature.SignerCertificate) { throw "Authenticode signature is not valid: $($Signature.Status)" }
    $actual = ([string]$Signature.SignerCertificate.Thumbprint -replace '\s', '').ToUpperInvariant()
    if ($actual -ne $thumbprint) { throw 'Authenticode signer thumbprint does not match the expected certificate thumbprint.' }
    if ($null -eq $Signature.TimeStamperCertificate) { throw 'Authenticode signature has no RFC3161 timestamp certificate evidence.' }
    [ordered]@{
        status = [string]$Signature.Status
        status_message = [string]$Signature.StatusMessage
        signer_thumbprint = $actual
        signer_subject = [string]$Signature.SignerCertificate.Subject
        signer_issuer = [string]$Signature.SignerCertificate.Issuer
        signer_serial = [string]$Signature.SignerCertificate.SerialNumber
        timestamp_thumbprint = (([string]$Signature.TimeStamperCertificate.Thumbprint -replace '\s', '').ToUpperInvariant())
        timestamp_subject = [string]$Signature.TimeStamperCertificate.Subject
        timestamp_issuer = [string]$Signature.TimeStamperCertificate.Issuer
        timestamp_serial = [string]$Signature.TimeStamperCertificate.SerialNumber
        verified_utc = [DateTimeOffset]::UtcNow.ToString('o')
    }
}

function Get-RelativeFilePath {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Path)
    $relative = [IO.Path]::GetRelativePath((Get-NormalizedFullPath $Root), (Get-NormalizedFullPath $Path)).Replace('\', '/')
    if ($relative -eq '..' -or $relative.StartsWith('../')) { throw "Path escapes root: $Path" }
    $relative
}

function Assert-ExactFileSet {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string[]]$AllowedRelativePaths)
    $rootPath = Get-NormalizedFullPath $Root
    if (-not (Test-Path -LiteralPath $rootPath -PathType Container)) { throw "Allowlist root is missing: $rootPath" }
    Assert-NoReparseAncestors $rootPath
    $allowed = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($relative in $AllowedRelativePaths) {
        $normalized = $relative.Replace('\', '/').Trim('/')
        if ([string]::IsNullOrWhiteSpace($normalized) -or [IO.Path]::IsPathFullyQualified($relative) -or $normalized.Split('/') -contains '..') { throw "Invalid allowlist path: $relative" }
        if (-not $allowed.Add($normalized)) { throw "Duplicate allowlist path: $normalized" }
    }
    $actual = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $entries = @(Get-ReparseSafeTreeEntries -Root $rootPath)
    foreach ($file in $entries | Where-Object { -not $_.PSIsContainer }) {
        $relative = Get-RelativeFilePath -Root $rootPath -Path $file.FullName
        if (-not $allowed.Contains($relative)) { throw "Unexpected file under deny-by-default allowlist: $relative" }
        $actual.Add($relative) | Out-Null
    }
    foreach ($relative in $allowed) {
        if (-not $actual.Contains($relative)) { throw "Allowlisted file is missing: $relative" }
    }
}

function Assert-InstalledTreeContract {
    param(
        [Parameter(Mandatory)][string]$InstallRoot,
        [Parameter(Mandatory)][string]$ExpectedExeSha256,
        [Parameter(Mandatory)][hashtable]$ResourceHashes,
        [Parameter(Mandatory)][object]$Signature,
        [Parameter(Mandatory)][string]$ExpectedThumbprint,
        [string]$UnprotectedSha256
    )
    $root = Get-NormalizedFullPath $InstallRoot
    $exe = Resolve-FullyQualifiedLeaf (Join-Path $root 'nwflash-desktop.exe')
    $exeHash = Get-Sha256Hex $exe
    if ($exeHash -ne $ExpectedExeSha256.ToUpperInvariant()) { throw 'Installed EXE hash does not equal the signed protected EXE.' }
    if (-not [string]::IsNullOrWhiteSpace($UnprotectedSha256) -and $exeHash -eq $UnprotectedSha256.ToUpperInvariant()) { throw 'Installed EXE equals the unprotected input hash.' }
    Assert-AuthenticodeIdentity -Signature $Signature -ExpectedThumbprint $ExpectedThumbprint | Out-Null
    $allowed = @('nwflash-desktop.exe', 'uninstall.exe') + @($ResourceHashes.Keys)
    foreach ($relative in $ResourceHashes.Keys) {
        $path = Resolve-FullyQualifiedLeaf (Join-Path $root $relative.Replace('/', '\'))
        if ((Get-Sha256Hex $path) -ne ([string]$ResourceHashes[$relative]).ToUpperInvariant()) { throw "Installed resource hash mismatch: $relative" }
    }
    Assert-ExactFileSet -Root $root -AllowedRelativePaths $allowed
}

function Assert-ConsoleExecutable {
    param([Parameter(Mandatory)][string]$Path)
    throw 'Automated VMProtect console execution is disabled; the Lite GUI handoff is manual.'
}

function Invoke-ProtectedReleasePipeline {
    param([Parameter(Mandatory)][Collections.IDictionary]$Operations)
    foreach ($name in @('accept', 'copy', 'sign-exe', 'bundle', 'sign-installer', 'install-compare', 'verify', 'manifest', 'verify-final')) {
        if (-not $Operations.Contains($name) -or $Operations[$name] -isnot [scriptblock]) { throw "Protected pipeline operation is missing: $name" }
        & $Operations[$name]
    }
}
