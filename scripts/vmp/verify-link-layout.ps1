[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SdkRoot
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
    [pscustomobject]@{ Symbol = 'nwflash_protection_build_identity_matches'; Mode = 'Mutation' }
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
        Write-Output "Marker region: $($leaf.Symbol) = $($leaf.Mode) + End"
    }
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

& (Join-Path $PSScriptRoot 'verify-sdk.ps1') -SdkRoot $resolvedSdkRoot | Write-Output
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
& cargo @cargoArguments
$cargoSucceeded = $?
if (-not $cargoSucceeded) {
    throw 'Full-link VMProtect release probe failed to build.'
}

foreach ($artifact in @($executable, $pdb, $map)) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf) -or (Get-Item -LiteralPath $artifact).Length -eq 0) {
        throw "Required release layout artifact is missing or empty: $artifact"
    }
}

$dumpbin = Resolve-X64Dumpbin
Assert-FinalPeImports -Dumpbin $dumpbin -Executable $executable
Write-Output 'Final PE imports: VMProtectSDK64.dll and 8 required symbols verified'
Assert-MarkerLayout -Dumpbin $dumpbin -Executable $executable -MapPath $map
Write-Output "Release layout artifacts: EXE, PDB, and MAP verified under $artifactRoot"
Write-Output 'No VMProtect SDK file was copied. Lite GUI and post-protection CRC verification remain Task 8.'
