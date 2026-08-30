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

$ExpectedSdkDll = 'VMProtectSDK64.dll'
$RequiredImports = @(
    'VMProtectBeginVirtualization',
    'VMProtectBeginMutation',
    'VMProtectBeginUltra',
    'VMProtectEnd',
    'VMProtectIsProtected',
    'VMProtectIsDebuggerPresent',
    'VMProtectIsVirtualMachinePresent',
    'VMProtectIsValidImageCRC'
)
$LeafContracts = @(
    [pscustomobject]@{ Symbol = 'nwflash_protection_accept_login_lease'; Mode = 'Ultra' },
    [pscustomobject]@{ Symbol = 'nwflash_protection_classify_heartbeat_lease'; Mode = 'Virtualization' },
    [pscustomobject]@{ Symbol = 'nwflash_protection_admit_local_operation'; Mode = 'Ultra' },
    [pscustomobject]@{ Symbol = 'nwflash_protection_verify_image_integrity'; Mode = 'Virtualization' },
    [pscustomobject]@{ Symbol = 'nwflash_protection_build_identity_matches'; Mode = 'Mutation' },
    [pscustomobject]@{ Symbol = 'nwflash_protection_trace_credential_sentinel'; Mode = 'Ultra' }
)

function Resolve-X64Dumpbin {
    $vswhereCandidates = @(
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe",
        "$env:ProgramFiles\Microsoft Visual Studio\Installer\vswhere.exe"
    )
    $vswhere = $vswhereCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (-not $vswhere) {
        throw 'vswhere.exe was not found; cannot inspect the linked PE.'
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

function Assert-FinalPeImports {
    param([string]$Dumpbin, [string]$Executable)

    $lines = @(& $Dumpbin /NOLOGO /IMPORTS $Executable 2>&1)
    $dumpbinSucceeded = $?
    if (-not $dumpbinSucceeded) {
        throw "dumpbin failed to inspect final PE imports: $($lines -join [Environment]::NewLine)"
    }
    $dllIndexes = @(
        for ($index = 0; $index -lt $lines.Count; $index++) {
            if ($lines[$index].Trim().Equals($ExpectedSdkDll, [System.StringComparison]::OrdinalIgnoreCase)) {
                $index
            }
        }
    )
    if ($dllIndexes.Count -ne 1) {
        throw "Final PE must contain exactly one $ExpectedSdkDll import block; found $($dllIndexes.Count)."
    }
    $start = $dllIndexes[0]
    $end = $lines.Count
    for ($index = $start + 1; $index -lt $lines.Count; $index++) {
        if ($lines[$index] -match '^\s{4}\S+\.dll\s*$') {
            $end = $index
            break
        }
    }
    $block = ($lines[$start..($end - 1)] -join [Environment]::NewLine)
    foreach ($symbol in $RequiredImports) {
        if ($block -notmatch "(?m)^\s+[0-9A-F]+\s+$([regex]::Escape($symbol))\s*$") {
            throw "Final PE $ExpectedSdkDll block is missing required import $symbol."
        }
    }
    $importedVmProtectNames = [regex]::Matches(
        $block,
        '(?m)^\s+[0-9A-F]+\s+(VMProtect\S+)\s*$'
    ) | ForEach-Object { $_.Groups[1].Value }
    foreach ($name in $importedVmProtectNames) {
        if ($RequiredImports -notcontains $name) {
            throw "Final PE imports unexpected VMProtect symbol $name."
        }
    }
    if ($importedVmProtectNames.Count -ne $RequiredImports.Count) {
        throw "Final PE VMProtect import count is $($importedVmProtectNames.Count); expected $($RequiredImports.Count)."
    }
}

function Get-PhysicalFunctionRegion {
    param([string[]]$Disassembly, [string]$Symbol)

    $label = "${Symbol}:"
    $starts = @(
        for ($index = 0; $index -lt $Disassembly.Count; $index++) {
            if ($Disassembly[$index] -ceq $label) {
                $index
            }
        }
    )
    if ($starts.Count -ne 1) {
        throw "Disassembly must contain exactly one $Symbol label; found $($starts.Count)."
    }
    $start = $starts[0]
    $end = $Disassembly.Count
    for ($index = $start + 1; $index -lt $Disassembly.Count; $index++) {
        if ($Disassembly[$index] -match '^\S.*:$') {
            $end = $index
            break
        }
    }
    return ($Disassembly[$start..($end - 1)] -join [Environment]::NewLine)
}

function Assert-MarkerLayout {
    param([string]$Dumpbin, [string]$Executable, [string]$MapPath)

    $mapText = Get-Content -LiteralPath $MapPath -Raw
    $disassembly = @(& $Dumpbin /NOLOGO /DISASM $Executable 2>&1)
    $dumpbinSucceeded = $?
    if (-not $dumpbinSucceeded) {
        throw "dumpbin failed to disassemble the release probe: $($disassembly -join [Environment]::NewLine)"
    }

    $verified = @()
    foreach ($leaf in $LeafContracts) {
        $mapCount = [regex]::Matches(
            $mapText,
            "(?im)^\s+[0-9A-F]{4}:[0-9A-F]{8}\s+$([regex]::Escape($leaf.Symbol))\s+"
        ).Count
        if ($mapCount -ne 1) {
            throw "MAP must contain exactly one $($leaf.Symbol) physical symbol; found $mapCount."
        }

        $region = Get-PhysicalFunctionRegion -Disassembly $disassembly -Symbol $leaf.Symbol
        $beginCalls = [regex]::Matches(
            $region,
            '(?m)\bcall\s+(?:qword ptr \[__imp_)?VMProtectBegin(Ultra|Virtualization|Mutation)\]?\s*$'
        )
        $endCalls = [regex]::Matches(
            $region,
            '(?m)\bcall\s+(?:qword ptr \[__imp_)?VMProtectEnd\]?\s*$'
        )
        if ($beginCalls.Count -ne 1) {
            throw "$($leaf.Symbol) physical region has $($beginCalls.Count) Begin calls; expected exactly one."
        }
        if ($beginCalls[0].Groups[1].Value -ne $leaf.Mode) {
            throw "$($leaf.Symbol) uses Begin$($beginCalls[0].Groups[1].Value); expected Begin$($leaf.Mode)."
        }
        if ($endCalls.Count -ne 1) {
            throw "$($leaf.Symbol) physical region has $($endCalls.Count) End calls; expected exactly one."
        }
        if ($beginCalls[0].Index -ge $endCalls[0].Index) {
            throw "$($leaf.Symbol) marker End does not physically follow Begin$($leaf.Mode)."
        }
        $protectedBody = $region.Substring($beginCalls[0].Index, $endCalls[0].Index - $beginCalls[0].Index)
        if ($protectedBody -match '(?im)\bret[nq]?\b') {
            throw "$($leaf.Symbol) returns before VMProtectEnd."
        }
        if ($leaf.Symbol -eq 'nwflash_protection_trace_credential_sentinel') {
            $innerStart = $beginCalls[0].Index + $beginCalls[0].Length
            $innerBody = $region.Substring($innerStart, $endCalls[0].Index - $innerStart)
            if ($innerBody -match '(?im)\bcall\b') {
                throw "$($leaf.Symbol) must not call helpers inside the Ultra marker region."
            }
        }
        $verified += [pscustomobject]@{ symbol = $leaf.Symbol; mode = $leaf.Mode; begin_count = 1; end_count = 1; verified = $true }
    }
    $verified
}

if (-not [System.IO.Path]::IsPathFullyQualified($SdkRoot)) {
    throw 'NWFLASH_VMP_SDK_ROOT must be a fully qualified path.'
}
$resolvedSdkRoot = (Resolve-Path -LiteralPath $SdkRoot).ProviderPath
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).ProviderPath
$manifestPath = Join-Path $repositoryRoot 'src\Nwflash.Desktop\src-tauri\Cargo.toml'
$targetRoot = Join-Path $repositoryRoot 'src\Nwflash.Desktop\src-tauri\target'
$artifactRoot = Join-Path $targetRoot 'release\examples'
$executable = Join-Path $artifactRoot 'vmp_link_probe.exe'
$pdb = Join-Path $artifactRoot 'vmp_link_probe.pdb'
$map = Join-Path $artifactRoot 'vmp_link_probe.map'

$sdkResult = (& (Join-Path $PSScriptRoot 'verify-sdk.ps1') -SdkRoot $resolvedSdkRoot -AsJson | ConvertFrom-Json)
$env:NWFLASH_VMP_SDK_ROOT = $resolvedSdkRoot
$cargoArguments = @(
    'rustc',
    '--manifest-path', $manifestPath,
    '--target-dir', $targetRoot,
    '-p', 'nwflash-protection',
    '--release',
    '--example', 'vmp_link_probe',
    '--features', 'vmp-sdk',
    '--',
    '-C', 'panic=abort',
    '-C', 'debuginfo=2',
    '-C', "link-arg=/MAP:$map",
    '-C', 'link-arg=/MAPINFO:EXPORTS',
    '-C', 'link-arg=/DEBUG:FULL'
)
$cargoOutput = (& cargo @cargoArguments 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { throw "Full-link VMProtect release probe failed to build: $cargoOutput" }

foreach ($artifact in @($executable, $pdb, $map)) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf) -or (Get-Item -LiteralPath $artifact).Length -eq 0) {
        throw "Required release layout artifact is missing or empty: $artifact"
    }
}

$dumpbin = Resolve-X64Dumpbin
Assert-FinalPeImports -Dumpbin $dumpbin -Executable $executable
$markerResult = @(Assert-MarkerLayout -Dumpbin $dumpbin -Executable $executable -MapPath $map)
$result = [ordered]@{
    schema = 1
    verified = $true
    machine = 'AMD64'
    sdk = $sdkResult
    imported_dll = $ExpectedSdkDll
    required_imports = @($RequiredImports)
    markers = $markerResult
    executable_sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToUpperInvariant()
    pdb_sha256 = (Get-FileHash -LiteralPath $pdb -Algorithm SHA256).Hash.ToUpperInvariant()
    map_sha256 = (Get-FileHash -LiteralPath $map -Algorithm SHA256).Hash.ToUpperInvariant()
    files_copied = 0
}
if ($AsJson) {
    $result | ConvertTo-Json -Depth 8 -Compress
}
else {
    Write-Output 'Final PE imports: VMProtectSDK64.dll and 8 required symbols verified'
    foreach ($marker in $markerResult) { Write-Output "Marker region: $($marker.symbol) = $($marker.mode) + End" }
    Write-Output "Release layout artifacts: EXE, PDB, and MAP verified under $artifactRoot"
    Write-Output 'No VMProtect SDK file was copied. Lite GUI and post-protection CRC verification remain Task 8.'
}
