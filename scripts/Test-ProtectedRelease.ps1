[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

foreach ($path in @('packaging\vmprotect\nwflash.vmp', 'scripts\Protect-NwflashRelease.ps1', 'scripts\Sign-NwflashRelease.ps1', 'scripts\Verify-ProtectedRelease.ps1', 'scripts\New-TauriReleaseManifest.ps1', 'scripts\Test-TauriInstaller.ps1', 'docs\release\tauri-vmp-signing-runbook.md')) {
    Assert-Condition (Test-Path (Join-Path $repo $path)) "Missing protected-release artifact: $path"
}

& (Join-Path $PSScriptRoot 'Verify-ProtectedRelease.ps1') -DryRun
Assert-Condition ($LASTEXITCODE -eq 0) 'Protected-release dry run must succeed without credentials.'

$config = Get-Content -Raw (Join-Path $repo 'packaging\vmprotect\nwflash.vmp')
Assert-Condition (-not $config.Contains('license')) 'VMProtect project must not contain a license.'
Assert-Condition (-not $config.Contains('thumbprint')) 'VMProtect project must not contain a certificate thumbprint.'
Assert-Condition (-not $config.Contains('C:\')) 'VMProtect project must not contain an absolute machine path.'
$signingScript = Get-Content -Raw (Join-Path $PSScriptRoot 'Sign-NwflashRelease.ps1')
Assert-Condition ($signingScript.Contains('Windows Kits')) 'Signing script must discover the Windows SDK SignTool when no explicit path is supplied.'
Assert-Condition ($signingScript.Contains('SignerCertificate.Thumbprint')) 'Signing script must verify the selected certificate identity.'
$protectedVerifier = Get-Content -Raw (Join-Path $PSScriptRoot 'Verify-ProtectedRelease.ps1')
Assert-Condition ($protectedVerifier.Contains('SignerCertificate.Thumbprint')) 'Protected verifier must verify the expected certificate identity.'

$priorVmpPath = $env:NWFLASH_VMP_PATH
$priorVmpProject = $env:NWFLASH_VMP_PROJECT
$priorVmpArguments = $env:NWFLASH_VMP_ARGUMENTS
$priorThumbprint = $env:NWFLASH_CERT_THUMBPRINT
$noOpRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-vmp-no-op-" + [Guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Force $noOpRoot | Out-Null
    Set-Content -Encoding ASCII -LiteralPath (Join-Path $noOpRoot 'nwflash-desktop.exe') -Value 'unprotected executable fixture'
    $noOpProject = Join-Path $noOpRoot 'controlled-project.vmp'
    Set-Content -Encoding ASCII -LiteralPath $noOpProject -Value 'controlled VMProtect project fixture'
    $env:NWFLASH_VMP_PATH = $env:ComSpec
    $env:NWFLASH_VMP_PROJECT = $noOpProject
    $env:NWFLASH_VMP_ARGUMENTS = '["/c","exit 0","{project}","{input}","{output}"]'
    $noOpProtectorRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'Protect-NwflashRelease.ps1') -ReleaseRoot $noOpRoot
    } catch {
        $noOpProtectorRejected = $_.Exception.Message -like '*did not produce protected output*'
    }
    Assert-Condition $noOpProtectorRejected 'A successful VMProtect process without a protected output must be rejected.'
} finally {
    if ($null -eq $priorVmpPath) { Remove-Item Env:NWFLASH_VMP_PATH -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_PATH = $priorVmpPath }
    if ($null -eq $priorVmpProject) { Remove-Item Env:NWFLASH_VMP_PROJECT -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_PROJECT = $priorVmpProject }
    if ($null -eq $priorVmpArguments) { Remove-Item Env:NWFLASH_VMP_ARGUMENTS -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_ARGUMENTS = $priorVmpArguments }
    if ($null -eq $priorThumbprint) { Remove-Item Env:NWFLASH_CERT_THUMBPRINT -ErrorAction SilentlyContinue } else { $env:NWFLASH_CERT_THUMBPRINT = $priorThumbprint }
    if (Test-Path $noOpRoot) { Remove-Item -LiteralPath $noOpRoot -Recurse -Force }
}

$publishScript = Get-Content -Raw (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1')
Assert-Condition ($publishScript.Contains('DevelopmentUnsigned')) 'Publish script must require an explicit development-unsigned override.'
Assert-Condition (-not $publishScript.Contains('ProtectAndSign')) 'Publish script must not expose an optional protected-release path.'
Assert-Condition ($publishScript.Contains('--no-bundle')) 'Protected-release path must build the unbundled EXE before VMProtect.'
Assert-Condition ($publishScript.Contains('Protect-NwflashRelease.ps1')) 'Protected-release path must run VMProtect before signing.'
Assert-Condition ($publishScript.Contains('Sign-NwflashRelease.ps1')) 'Protected-release path must sign the EXE and NSIS installer.'
Assert-Condition ($publishScript.Contains('Verify-ProtectedRelease.ps1')) 'Protected-release path must verify the signed release.'
Assert-Condition ($publishScript.Contains('Test-TauriInstaller.ps1')) 'Protected release must test the final installer.'
Assert-Condition ($publishScript.Contains('test:native')) 'Protected release must run native Tauri E2E tests.'

$failingCargoDirectory = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-failing-cargo-" + [Guid]::NewGuid().ToString('N'))
$failingCargoReleaseRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-failing-cargo-release-" + [Guid]::NewGuid().ToString('N'))
$priorPath = $env:PATH
$priorVmpPath = $env:NWFLASH_VMP_PATH
$priorVmpProject = $env:NWFLASH_VMP_PROJECT
$priorVmpArguments = $env:NWFLASH_VMP_ARGUMENTS
$priorThumbprint = $env:NWFLASH_CERT_THUMBPRINT
try {
    New-Item -ItemType Directory -Force $failingCargoDirectory | Out-Null
    Set-Content -Encoding ASCII -LiteralPath (Join-Path $failingCargoDirectory 'cargo.cmd') -Value '@exit /b 17'
    $controlledProject = Join-Path $failingCargoDirectory 'controlled-project.vmp'
    Set-Content -Encoding ASCII -LiteralPath $controlledProject -Value 'controlled VMProtect project fixture'
    $env:PATH = "$failingCargoDirectory;$priorPath"
    $env:NWFLASH_VMP_PATH = $env:ComSpec
    $env:NWFLASH_VMP_PROJECT = $controlledProject
    $env:NWFLASH_VMP_ARGUMENTS = '["/c","exit 0","{project}","{input}","{output}"]'
    $env:NWFLASH_CERT_THUMBPRINT = '00112233445566778899AABBCCDDEEFF00112233'
    $cargoFailureRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -SkipBuild -ReleaseRoot $failingCargoReleaseRoot
    } catch {
        $cargoFailureRejected = $_.Exception.Message -like '*Rust workspace tests failed with exit code 17*'
    }
    Assert-Condition $cargoFailureRejected 'Protected release must stop at a failed Rust workspace test command.'
    Assert-Condition (-not (Get-ChildItem $failingCargoReleaseRoot -File -Filter '*-setup.exe' -ErrorAction SilentlyContinue)) 'Protected release published an installer after a failed Rust workspace test command.'
} finally {
    $env:PATH = $priorPath
    if ($null -eq $priorVmpPath) { Remove-Item Env:NWFLASH_VMP_PATH -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_PATH = $priorVmpPath }
    if ($null -eq $priorVmpProject) { Remove-Item Env:NWFLASH_VMP_PROJECT -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_PROJECT = $priorVmpProject }
    if ($null -eq $priorVmpArguments) { Remove-Item Env:NWFLASH_VMP_ARGUMENTS -ErrorAction SilentlyContinue } else { $env:NWFLASH_VMP_ARGUMENTS = $priorVmpArguments }
    if ($null -eq $priorThumbprint) { Remove-Item Env:NWFLASH_CERT_THUMBPRINT -ErrorAction SilentlyContinue } else { $env:NWFLASH_CERT_THUMBPRINT = $priorThumbprint }
    if (Test-Path $failingCargoDirectory) { Remove-Item -LiteralPath $failingCargoDirectory -Recurse -Force }
    if (Test-Path $failingCargoReleaseRoot) { Remove-Item -LiteralPath $failingCargoReleaseRoot -Recurse -Force }
}

$protectedRoot = Join-Path ([IO.Path]::GetTempPath()) ("nwflash-tauri-protected-test-" + [Guid]::NewGuid().ToString('N'))
try {
    $protectionRejected = $false
    try {
        & (Join-Path $PSScriptRoot 'Publish-TauriRelease.ps1') -SkipBuild -ReleaseRoot $protectedRoot
    } catch {
        $protectionRejected = $_.Exception.Message -like '*NWFLASH_VMP_PATH is required for a protected release*'
    }
    Assert-Condition $protectionRejected 'Protected release must fail closed when VMProtect is unavailable.'
    Assert-Condition (-not (Get-ChildItem $protectedRoot -File -Filter '*-setup.exe' -ErrorAction SilentlyContinue)) 'Protected release published an installer before VMProtect completed.'
} finally {
    if (Test-Path $protectedRoot) {
        Remove-Item -LiteralPath $protectedRoot -Recurse -Force
    }
}

Write-Host 'Protected release contract passed.'
