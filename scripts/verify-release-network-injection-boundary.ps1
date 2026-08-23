$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
$infrastructurePath = (Resolve-Path -LiteralPath (
    Join-Path $repoRoot "src\Nwflash.Desktop\src-tauri\crates\nwflash-infrastructure"
)).Path.Replace("\", "/")
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$probeRoot = [IO.Path]::GetFullPath((Join-Path $tempRoot (
    "nwflash-release-network-probe-" + [Guid]::NewGuid().ToString("N")
)))
if (-not $probeRoot.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Release probe path escaped the system temporary directory."
}

try {
    $sourceRoot = Join-Path $probeRoot "src"
    New-Item -ItemType Directory -Path $sourceRoot -Force | Out-Null
    $manifest = @"
[package]
name = "nwflash-release-network-probe"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
nwflash-infrastructure = { path = "$infrastructurePath" }
"@
    [IO.File]::WriteAllText((Join-Path $probeRoot "Cargo.toml"), $manifest)
    [IO.File]::WriteAllText(
        (Join-Path $sourceRoot "main.rs"),
        @"
use nwflash_infrastructure::{
    ApiTlsPolicy, AuthService, CloudflareClient, PinnedApiClient, VersionClient,
};

fn main() {
    let _ = CloudflareClient::new_injected("http://127.0.0.1:1", "test");
    let _: Option<ApiTlsPolicy> = None;
    let _: Option<PinnedApiClient> = None;
    let _ = AuthService::new("http://127.0.0.1:1", "test");
    let _ = VersionClient::new("http://127.0.0.1:1", "test");
}
"@
    )

    $previousTargetDir = $env:CARGO_TARGET_DIR
    $env:CARGO_TARGET_DIR = Join-Path $probeRoot "target"
    try {
        $output = (& cargo check --manifest-path (Join-Path $probeRoot "Cargo.toml") `
            --release --offline 2>&1 | Out-String)
        $exitCode = $LASTEXITCODE
    }
    finally {
        if ($null -eq $previousTargetDir) {
            Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
        }
        else {
            $env:CARGO_TARGET_DIR = $previousTargetDir
        }
    }

    if ($exitCode -eq 0) {
        throw "Release build still exposes custom TLS/HTTP injection symbols."
    }
    $requiredDiagnostics = @(
        "ApiTlsPolicy",
        "PinnedApiClient",
        "new_injected",
        "AuthService",
        "VersionClient"
    )
    if ($requiredDiagnostics.Where({ $output -notmatch $_ }).Count -ne 0) {
        throw "Release probe failed for an unrelated reason:`n$output"
    }
    Write-Output "Release network injection boundary enforced."
}
finally {
    if (Test-Path -LiteralPath $probeRoot) {
        Remove-Item -LiteralPath $probeRoot -Recurse -Force
    }
}

$global:LASTEXITCODE = 0
