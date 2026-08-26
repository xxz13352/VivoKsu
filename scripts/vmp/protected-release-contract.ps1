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
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
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
    Assert-PathNotReparsePoint $fullPath | Out-Null
    $item = Get-Item -LiteralPath $fullPath -Force
    if (-not $AllowEmpty -and $item.Length -le 0) {
        throw "Required file is empty: $fullPath"
    }
    $item.FullName
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
    if (Test-Path -LiteralPath $fullPath) { throw "Evidence already exists and will not be overwritten: $fullPath" }
    $directory = Split-Path -Parent $fullPath
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) { throw "Evidence directory is missing: $directory" }
    Assert-PathNotReparsePoint $directory | Out-Null
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
    (Get-Item -LiteralPath $fullPath).IsReadOnly = $true
    $fullPath
}

function Read-ProtectedEvidence {
    param([Parameter(Mandatory)][string]$Path, [string]$ExpectedState)
    $leaf = Resolve-FullyQualifiedLeaf $Path
    if (-not (Get-Item -LiteralPath $leaf).IsReadOnly) { throw "Evidence is not immutable/read-only: $leaf" }
    try { $document = Get-Content -Raw -LiteralPath $leaf | ConvertFrom-Json } catch { throw "Evidence is not valid JSON: $leaf" }
    if ($document.schema -ne 1) { throw "Evidence schema is unsupported: $leaf" }
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
        GetSignature = { param($Path) Get-AuthenticodeSignature -LiteralPath $Path }
        AssertMatchingPdb = { param($Exe, $Pdb) Assert-MatchingPdb -Executable $Exe -Pdb $Pdb }
        AssertMarkerLayout = { param($Exe, $Map) Assert-DesktopMarkerLayout -Executable $Exe -MapPath $Map }
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
    Assert-Amd64Pe $exe | Out-Null
    $signature = & $Operations.GetSignature $exe
    if ([string]$signature.Status -ne 'NotSigned') { throw "Pre-VMP input must be unsigned; status was $($signature.Status)." }
    & $Operations.AssertMatchingPdb $exe $pdb
    $markerLayout = & $Operations.AssertMarkerLayout $exe $map

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

    $sdkResult = & $Operations.VerifySdk $environment.sdk_root
    $linkResult = & $Operations.VerifyLinkLayout $environment.sdk_root
    $handoffId = [Guid]::NewGuid().ToString('D')
    $handoffDirectory = Join-Path $handoffBase $handoffId
    if (Test-Path -LiteralPath $handoffDirectory) { throw "Handoff directory already exists: $handoffDirectory" }
    $inputDirectory = Join-Path $handoffDirectory 'input'
    $evidenceDirectory = Join-Path $handoffDirectory 'evidence'
    New-Item -ItemType Directory -Path $inputDirectory | Out-Null
    New-Item -ItemType Directory -Path $evidenceDirectory | Out-Null
    $sdkEvidence = Write-AtomicEvidence -Path (Join-Path $evidenceDirectory 'sdk-verification.json') -Value $sdkResult
    $linkEvidence = Write-AtomicEvidence -Path (Join-Path $evidenceDirectory 'link-layout.json') -Value $linkResult

    $copiedExe = Join-Path $inputDirectory 'nwflash-desktop.exe'
    $copiedPdb = Join-Path $inputDirectory 'nwflash-desktop.pdb'
    $copiedMap = Join-Path $inputDirectory 'nwflash-desktop.map'
    Copy-Item -LiteralPath $exe -Destination $copiedExe
    Copy-Item -LiteralPath $pdb -Destination $copiedPdb
    Copy-Item -LiteralPath $map -Destination $copiedMap
    foreach ($pair in @(@($exe, $copiedExe), @($pdb, $copiedPdb), @($map, $copiedMap))) {
        if ((Get-Sha256Hex $pair[0]) -ne (Get-Sha256Hex $pair[1]) -or (Get-Item -LiteralPath $pair[0]).Length -ne (Get-Item -LiteralPath $pair[1]).Length) {
            throw "Handoff copy hash/length mismatch: $($pair[1])"
        }
    }
    & $Operations.AssertMatchingPdb $copiedExe $copiedPdb
    & $Operations.AssertMarkerLayout $copiedExe $copiedMap | Out-Null
    foreach ($path in @($copiedExe, $copiedPdb, $copiedMap)) { (Get-Item -LiteralPath $path).IsReadOnly = $true }

    $gitCommit = (& git -C $script:NwflashRepositoryRoot rev-parse HEAD).Trim().ToLowerInvariant()
    if ($LASTEXITCODE -ne 0 -or $gitCommit -notmatch '^[0-9a-f]{40}$') { throw 'Unable to record the exact Git commit for the handoff.' }
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
    param([Parameter(Mandatory)][object]$Review, [Parameter(Mandatory)][object]$Prepared, [Parameter(Mandatory)][string]$PreparedHash, [Parameter(Mandatory)][string]$CompilerLogHash)
    if ($Review.schema -ne 1 -or [string]$Review.handoff_id -ne [string]$Prepared.handoff_id) { throw 'Marker review schema or handoff ID is invalid.' }
    if ([string]$Review.prepared_manifest_sha256 -ne $PreparedHash -or [string]$Review.compiler_log_sha256 -ne $CompilerLogHash) { throw 'Marker review hash binding is invalid.' }
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
    param([Parameter(Mandatory)][object]$Result, [Parameter(Mandatory)][string]$ExpectedSha256)
    $exitCode = [int]$Result.exit_code
    if ($exitCode -ne 0) { throw "Protected runtime probe failed with exit code $exitCode." }
    if (-not [bool]$Result.probe_available -or -not [bool]$Result.VMProtectIsProtected -or -not [bool]$Result.VMProtectIsValidImageCRC) {
        throw 'Protected runtime probe did not return valid protection and CRC signals.'
    }
    if (-not ([string]$Result.observed_sha256).Equals($ExpectedSha256, [StringComparison]::OrdinalIgnoreCase)) { throw 'Protected runtime probe hash does not match the accepted output.' }
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
    Assert-MarkerReview -Review $review -Prepared $prepared -PreparedHash $preparedHash -CompilerLogHash $compilerHash
    $reviewHash = Get-Sha256Hex $reviewPath
    $probeResult = & $Operations.RunProbe $output $outputHash
    Assert-ProtectedProbeResult -Result $probeResult -ExpectedSha256 $outputHash
    if ((Get-Sha256Hex $output) -ne $outputHash) { throw 'Protected output changed while acceptance was running.' }

    foreach ($path in @($output, $compilerLog, $reviewPath)) { (Get-Item -LiteralPath $path).IsReadOnly = $true }
    $accepted = [ordered]@{
        schema = 1
        handoff_id = [string]$prepared.handoff_id
        state = 'accepted'
        created_utc = [DateTimeOffset]::UtcNow.ToString('o')
        previous_evidence_sha256 = $preparedHash
        prepared_manifest_sha256 = $preparedHash
        input_exe_sha256 = [string]$prepared.input_exe.sha256
        input_pdb_sha256 = [string]$prepared.input_pdb.sha256
        input_map_sha256 = [string]$prepared.input_map.sha256
        protected_output = [ordered]@{ path = $output; length = (Get-Item $output).Length; sha256 = $outputHash; machine = 'AMD64'; authenticode = 'NotSigned' }
        sdk_imports_present = $false
        compiler_log_sha256 = $compilerHash
        marker_review_sha256 = $reviewHash
        is_protected = $true
        image_crc_valid = $true
        runtime_probe_exit_code = 0
    }
    Write-AtomicEvidence -Path (Join-Path (Split-Path -Parent $preparedPath) 'accepted.json') -Value $accepted
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
    if (Test-Path -LiteralPath $fullPath) { Remove-Item -LiteralPath $fullPath -Recurse -Force }
}

function Invoke-IsolatedProtectedProbe {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$ExpectedSha256)
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
            $process = Start-Process -FilePath $probeExe -ArgumentList '--nwflash-protected-release-probe' -WorkingDirectory $root -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
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
    $allowed = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($relative in $AllowedRelativePaths) {
        $normalized = $relative.Replace('\', '/').Trim('/')
        if ([string]::IsNullOrWhiteSpace($normalized) -or [IO.Path]::IsPathFullyQualified($relative) -or $normalized.Split('/') -contains '..') { throw "Invalid allowlist path: $relative" }
        if (-not $allowed.Add($normalized)) { throw "Duplicate allowlist path: $normalized" }
    }
    $actual = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in Get-ChildItem -LiteralPath $rootPath -Recurse -File -Force) {
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
    $fullPath = Get-NormalizedFullPath $Path
    if ([IO.Path]::GetFileName($fullPath) -ine 'VMProtect_Con.exe') { throw 'Optional console automation requires a genuine VMProtect_Con.exe; the Lite GUI is manual.' }
    Resolve-FullyQualifiedLeaf $fullPath
}

function Invoke-ProtectedReleasePipeline {
    param([Parameter(Mandatory)][Collections.IDictionary]$Operations)
    foreach ($name in @('accept', 'copy', 'sign-exe', 'bundle', 'sign-installer', 'install-compare', 'verify', 'manifest', 'verify-final')) {
        if (-not $Operations.Contains($name) -or $Operations[$name] -isnot [scriptblock]) { throw "Protected pipeline operation is missing: $name" }
        & $Operations[$name]
    }
}
