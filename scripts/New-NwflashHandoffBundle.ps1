#requires -Version 7.4
#requires -PSEdition Core

<#
.SYNOPSIS
打包「换机继续」交接包：源码快照 + 交接文档 + VMP handoff 三件套 + 环境清单。

.DESCRIPTION
产出一个 zip，拿到另一台机器上可以不做任何前置阅读直接开工。内容与理由：

* `source/`            —— `git archive` 出的**整个仓库**已跟踪快照（沿用仓库内相对路径，
                          如 `source/src/Nwflash.Desktop/src-tauri/…`）。用 git archive
                          而非裸拷贝：不含 target/node_modules/.git，且带得上 commit 身份。
* `handoff/`           —— 当前有效的 VMP handoff 目录（EXE/MAP/PDB + evidence）。
* `docs/`              —— 本仓库 docs/ 下的交接与发布类文档（含会话接力）。
* `environment.env`    —— 复现用的环境变量清单（含编译期 option_env! 的值）。
* `PREREQUISITES.md`   —— 新机器必须自备的**外部**依赖与钉死哈希。
* `MANIFEST.json`      —— 每个文件的 SHA-256，落地后可逐个核对。
* `BUILD_INFO.txt`     —— 源码 commit、handoff id 与各输入哈希，便于对账。

**重要限制（脚本会打印警告）**：VMP handoff 的 `prepared.json` 里记录的是**绝对路径**
与本机 `git_commit`。跨机继续时该 handoff 仅作参考，**推荐在新机器重跑
`Publish-TauriRelease.ps1 -PrepareManual`** 产出该机自己的新鲜 handoff。详见
`docs/2026-09-29-session-handoff.md` §3.3。

.PARAMETER OutputDirectory
zip 落地的目录，默认 `<repo>/artifacts/handoff-bundle`。

.PARAMETER HandoffDirectory
要打进包里的 VMP handoff 目录。默认取 `artifacts/vmp-handoff/` 下**最新**的一个。

.PARAMETER Force
允许覆盖已存在的同名 zip。默认拒绝（与仓库其它打包脚本一致）。
#>
[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$HandoffDirectory,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot 'artifacts\handoff-bundle'
}

$stamp = (Get-Date).ToString('yyyyMMdd-HHmmss')
$archivePath = Join-Path $OutputDirectory "Nwflash-handoff-$stamp.zip"
$hashPath = "$archivePath.sha256"
$stageRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-handoff-bundle-" + [Guid]::NewGuid().ToString('N'))
$packagedHandoffId = $null
$sourceCommit = $null

function Get-RepoGitValue {
    param([Parameter(Mandatory)][string[]]$Arguments, [Parameter(Mandatory)][string]$Purpose)
    $value = (& git -C $repoRoot @Arguments 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "Unable to read $Purpose from git: $value" }
    $value
}

function Get-RelativePathFrom {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Path)
    $rootUri = [Uri](($Root.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar))
    [Uri]::UnescapeDataString($rootUri.MakeRelativeUri([Uri]$Path).ToString()).Replace('/', '\')
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)][string]$Path)
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

try {
    if (Test-Path -LiteralPath $archivePath) { throw "Refusing to overwrite an existing bundle: $archivePath" }

    # 未提交改动会让交接包与 commit 身份对不上，先拒绝。
    $dirty = @(& git -C $repoRoot status --porcelain)
    if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect the worktree before bundling.' }
    if ($dirty.Count -ne 0) {
        throw "Handoff bundle requires a clean worktree; commit or stash first:`n$($dirty -join "`n")"
    }

    $sourceCommit = Get-RepoGitValue -Arguments @('rev-parse', 'HEAD') -Purpose 'commit'
    $sourceDescribe = Get-RepoGitValue -Arguments @('describe', '--tags', '--always', '--dirty') -Purpose 'description'

    # 定位要打包的 handoff。
    if ([string]::IsNullOrWhiteSpace($HandoffDirectory)) {
        $handoffRoot = Join-Path $repoRoot 'artifacts\vmp-handoff'
        if (-not (Test-Path -LiteralPath $handoffRoot -PathType Container)) {
            throw "VMP handoff root is missing: $handoffRoot"
        }
        $candidates = @(Get-ChildItem -LiteralPath $handoffRoot -Directory -ErrorAction SilentlyContinue |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'evidence\prepared.json') } |
            Sort-Object Name -Descending)
        if ($candidates.Count -eq 0) { throw "No prepared VMP handoff was found under $handoffRoot." }
        $HandoffDirectory = $candidates[0].FullName
    }
    $handoffSource = (Resolve-Path -LiteralPath $HandoffDirectory).ProviderPath
    if (-not (Test-Path -LiteralPath (Join-Path $handoffSource 'evidence\prepared.json') -PathType Leaf)) {
        throw "Handoff directory has no evidence/prepared.json: $handoffSource"
    }
    $prepared = Get-Content -Raw -LiteralPath (Join-Path $handoffSource 'evidence\prepared.json') | ConvertFrom-Json
    $packagedHandoffId = [string]$prepared.handoff_id

    [IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
    [IO.Directory]::CreateDirectory($stageRoot) | Out-Null

    # 1) 源码快照：git archive 只含已跟踪内容，天然排除 target/node_modules/.git。
    $sourceStage = Join-Path $stageRoot 'source'
    [IO.Directory]::CreateDirectory($sourceStage) | Out-Null
    $sourceZip = Join-Path $stageRoot 'source.zip'
    & git -C $repoRoot archive --format=zip --output=$sourceZip $sourceCommit
    if ($LASTEXITCODE -ne 0) { throw 'git archive failed while snapshotting the source tree.' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($sourceZip, $sourceStage)
    Remove-Item -LiteralPath $sourceZip -Force

    # 2) VMP handoff 目录（含 EXE/MAP/PDB 与 evidence）。
    $handoffStage = Join-Path $stageRoot 'handoff'
    Copy-Item -LiteralPath $handoffSource -Destination $handoffStage -Recurse

    # 3) 交接/发布类文档。只挑与「接手开工」相关的，避免整仓 docs 塞进包。
    $docsStage = Join-Path $stageRoot 'docs'
    [IO.Directory]::CreateDirectory($docsStage) | Out-Null
    $docNames = @(
        'index.md',
        'project-architecture.md',
        'product-decisions.md',
        '2026-09-29-session-handoff.md',
        '2026-09-11-session-handoff.md',
        '2026-09-04-iteration-plan.md',
        '2026-09-04-release-gate-audit.md',
        '2026-09-26-payload-inhouse-and-release-blockers.md'
    )
    foreach ($name in $docNames) {
        $candidate = Join-Path $repoRoot "docs\$name"
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            Copy-Item -LiteralPath $candidate -Destination (Join-Path $docsStage $name)
        }
    }
    $runbook = Join-Path $repoRoot 'docs\release\tauri-vmp-signing-runbook.md'
    if (Test-Path -LiteralPath $runbook -PathType Leaf) {
        [IO.Directory]::CreateDirectory((Join-Path $docsStage 'release')) | Out-Null
        Copy-Item -LiteralPath $runbook -Destination (Join-Path $docsStage 'release\tauri-vmp-signing-runbook.md')
    }

    # 4) 根级说明文件。
    foreach ($name in @('README.md', 'PROJECT_PROGRESS.md')) {
        $candidate = Join-Path $repoRoot $name
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            Copy-Item -LiteralPath $candidate -Destination (Join-Path $stageRoot $name)
        }
    }

    # 5) 编译期环境变量：这些是 option_env! 值，缺了产物会静默退化成探针。
    $envLines = @(
        '# NWflash 受保护构建环境（换机后在新机器的新 shell 里逐行 export）',
        '# 注意：NWFLASH_* 里 SESSION_VERIFY_KEY_B64 与 BUILD_ID 是编译期 option_env!，',
        '# 缺失时 release 产物会静默退化成 ~1.5MB 探针，构建过程不报错。',
        '#',
        '# MSVC / Windows SDK 三件套：只给 LIB 会让 C 构建脚本失败；',
        '# 只给 INCLUDE 会让链接器撞上 Git Bash 的 coreutils link。',
        'export CARGO_INCREMENTAL=0',
        'export PATH="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64:$PATH"',
        "export LIB='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\lib\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\ucrt\x64'",
        "export INCLUDE='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\include;C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\ucrt;C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um;C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\shared'",
        "export NWFLASH_VMP_SDK_ROOT='C:\Users\17254\Downloads\VMProtect Lite v3.10.4 Build 2668 (1)'",
        "export NWFLASH_BUILD_ID='$([string]$prepared.build_id)'",
        "export NWFLASH_SESSION_VERIFY_KEY_B64='HSNEfWZrjbZRhspVBhjcVOPxWiJJmx7tHO7JVMMug8o='",
        "export NWFLASH_DUMPBIN_PATH='C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\dumpbin.exe'",
        '',
        "export NWFLASH_HANDOFF_ID='$packagedHandoffId'"
    )
    Set-Content -LiteralPath (Join-Path $stageRoot 'environment.env') -Value $envLines -Encoding utf8NoBOM

    # 6) 新机器必须自备的外部依赖（仓库不含，且哈希被发布钉死）。
    $prerequisites = @"
# 新机器必须自备的外部依赖

本仓库**不含**下列内容。SDK 三件套哈希与 `scripts/vmp/verify-sdk.ps1` 里钉死的值
逐字节比对，不符会被**直接拒收**（"a structurally similar or newer SDK is rejected"）。
SDK 与 license **不得**拷进仓库。

| 依赖 | 版本 | SHA-256 |
| --- | --- | --- |
| VMProtect Lite x64 header (`Include/C/VMProtectSDK.h`) | v3.10.4 Build 2668 | ``2300B7B4BB6BBF9CFA08013EC2D9B2FDCEB3DFD2E603CD1E24A493DE4D165B15`` |
| 导入库 `Lib/Windows/VMProtectSDK64.lib` | v3.10.4 Build 2668 | ``9997A9C6E179010450385832A66EA36938E180FC9067D91FD6AAE7C9F6BF4D18`` |
| SDK DLL `Lib/Windows/VMProtectSDK64.dll` | v3.10.4 Build 2668 | ``EC3235136A4DAEE2A6F72C0F2994A8365CA8427C8068D068130B74C9FA64CD02`` |
| MSVC BuildTools | 14.44.35207 | — |
| Windows SDK | 10.0.26100.0 | — |
| rustc / cargo | 1.98.1 | — |
| Node.js | 24.18.0 | — |
| PowerShell | ``pwsh`` ≥ 7.4（Windows PowerShell 5.1 **不支持**，``#requires`` 会直接失败） | — |

## 校验 SDK

``````bash
pwsh -NoProfile -File scripts/vmp/verify-sdk.ps1 -SdkRoot "`$NWFLASH_VMP_SDK_ROOT" -AsJson
pwsh -NoProfile -File scripts/vmp/test-contracts.ps1 -SdkRoot "`$NWFLASH_VMP_SDK_ROOT" -AsJson
``````

## 产物健康判据（必查，构建成功 != 产物可用）

| 检查 | 健康 | 残废 |
| --- | --- | --- |
| ``target/release/nwflash-desktop.exe`` 大小 | ~11.4 MB | ~1.5 MB |
| MAP 里 8 个 ``nwflash_protection_*`` | 8/8 | 0 或不更新 |
| 前端 bundle 名（如 ``index-DRY8zCgr.js``）在 exe 中 | 命中 | 0 |
| 生产公钥的 base64 文本在 exe 中 | 1 处 | 0 |

## 跨机使用 handoff 的注意

``handoff/evidence/prepared.json`` 记录的是**本机绝对路径**与 ``git_commit``，
且 ``artifacts/`` 不进 git。因此：

* **推荐**：在新机器重跑 ``Publish-TauriRelease.ps1 -PrepareManual``（前置检查全自动，
  约 10 分钟），产出该机自己的新鲜 handoff。
* 直接把本包的 ``handoff/`` 拷过去**不会生效**，除非新机器路径与本机完全一致。
"@
    Set-Content -LiteralPath (Join-Path $stageRoot 'PREREQUISITES.md') -Value $prerequisites -Encoding utf8NoBOM

    # 7) 构建身份信息，便于落地后对账。
    $buildInfo = @(
        "source_commit   = $sourceCommit"
        "source_describe = $sourceDescribe"
        "bundled_utc     = $([DateTimeOffset]::UtcNow.ToString('o'))"
        "handoff_id      = $packagedHandoffId"
        "handoff_commit  = $([string]$prepared.git_commit)"
        "build_id        = $([string]$prepared.build_id)"
        "input_exe_sha256= $([string]$prepared.input_exe.sha256)"
        "input_exe_length= $([string]$prepared.input_exe.length)"
        "input_map_sha256= $([string]$prepared.input_map.sha256)"
        "input_pdb_sha256= $([string]$prepared.input_pdb.sha256)"
    )
    Set-Content -LiteralPath (Join-Path $stageRoot 'BUILD_INFO.txt') -Value $buildInfo -Encoding utf8NoBOM

    # 8) MANIFEST.json：逐个文件的 SHA-256。
    $manifestEntries = [System.Collections.Generic.List[object]]::new()
    foreach ($file in (Get-ChildItem -LiteralPath $stageRoot -Recurse -File | Sort-Object FullName)) {
        $manifestEntries.Add([ordered]@{
                path   = (Get-RelativePathFrom -Root $stageRoot -Path $file.FullName).Replace('\', '/')
                length = $file.Length
                sha256 = Get-Sha256Hex -Path $file.FullName
            })
    }
    $manifest = [ordered]@{
        schema      = 1
        description = 'NWflash cross-machine handoff bundle'
        files       = $manifestEntries
    }
    $manifest | ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath (Join-Path $stageRoot 'MANIFEST.json') -Encoding utf8NoBOM

    Compress-Archive -Path (Join-Path $stageRoot '*') -DestinationPath $archivePath -CompressionLevel Optimal
    $archiveHash = Get-Sha256Hex -Path $archivePath
    Set-Content -LiteralPath $hashPath -Value ("$archiveHash  " + (Split-Path -Leaf $archivePath)) -Encoding ascii

    # 打包后自检：必需条目在、禁用条目不在。
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $entryNames = @($zip.Entries | ForEach-Object { $_.FullName })
        $requiredEntries = @(
            'environment.env',
            'PREREQUISITES.md',
            'BUILD_INFO.txt',
            'MANIFEST.json',
            'source/src/Nwflash.Desktop/src-tauri/Cargo.toml',
            'source/src/Nwflash.Desktop/src/app/App.tsx',
            'source/src/Nwflash.Desktop/src-tauri/resources/drivers/vivo-usb-driver.7z',
            "handoff/evidence/prepared.json"
        )
        $missing = @($requiredEntries | Where-Object { $_ -notin $entryNames })
        if ($missing.Count -gt 0) { throw "Bundle is missing required entries: $($missing -join ', ')" }
        $forbidden = @($entryNames | Where-Object {
                $_ -match '(?:^|/)(?:\.git|node_modules|target|dist|\.vite|logs|gen|coverage)(?:/|$)'
            })
        if ($forbidden.Count -gt 0) { throw "Bundle contains excluded entries: $($forbidden -join ', ')" }
        $archiveEntryCount = $entryNames.Count
    }
    finally {
        $zip.Dispose()
    }

    Write-Output "SOURCE_COMMIT=$sourceCommit"
    Write-Output "HANDOFF_ID=$packagedHandoffId"
    Write-Output "HANDOFF_COMMIT=$([string]$prepared.git_commit)"
    Write-Output "BUNDLE_PATH=$archivePath"
    Write-Output "BUNDLE_SHA256=$archiveHash"
    Write-Output "BUNDLE_ENTRIES=$archiveEntryCount"
    Write-Output "BUNDLE_BYTES=$((Get-Item -LiteralPath $archivePath).Length)"
    if ([string]$prepared.git_commit -ne $sourceCommit) {
        Write-Warning "Handoff commit ($([string]$prepared.git_commit)) differs from source commit ($sourceCommit) — the bundled handoff is stale; re-run -PrepareManual on this commit before protecting."
    }
}
finally {
    $resolvedStage = [IO.Path]::GetFullPath($stageRoot)
    $safeToDelete = $resolvedStage.StartsWith([IO.Path]::GetFullPath([IO.Path]::GetTempPath()), [StringComparison]::OrdinalIgnoreCase) -and
        $resolvedStage.Contains('nwflash-handoff-bundle-')
    if ($safeToDelete -and (Test-Path -LiteralPath $stageRoot)) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force
    }
}
