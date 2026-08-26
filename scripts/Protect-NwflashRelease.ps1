[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Console')][string]$Mode,
    [Parameter(Mandatory)][string]$PreparedManifest,
    [Parameter(Mandatory)][string]$ConsolePath,
    [Parameter(Mandatory)][string]$ProjectPath,
    [Parameter(Mandatory)][string[]]$ConsoleArguments,
    [Parameter(Mandatory)][string]$MarkerReviewPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'vmp\protected-release-contract.ps1')

$console = Assert-ConsoleExecutable -Path $ConsolePath
$project = Resolve-FullyQualifiedLeaf $ProjectPath
$repoPrefix = (Get-NormalizedFullPath $repo).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if ($project.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The VMProtect console project must remain external to the repository.'
}

$preparedPath = Resolve-FullyQualifiedLeaf $PreparedManifest
$prepared = Read-ProtectedEvidence -Path $preparedPath -ExpectedState 'prepared'
$input = Resolve-FullyQualifiedLeaf ([string]$prepared.input_exe.path)
$output = Get-NormalizedFullPath ([string]$prepared.protected_output_path)
$compilerLog = Get-NormalizedFullPath ([string]$prepared.compiler_log_path)
if (Test-Path -LiteralPath $output) { throw "Console output already exists and will not be overwritten: $output" }
if (Test-Path -LiteralPath $compilerLog) { throw "Compiler log already exists and will not be overwritten: $compilerLog" }

$templates = @($ConsoleArguments)
if ($templates.Count -eq 0) { throw 'ConsoleArguments must be a non-empty argument array.' }
foreach ($placeholder in @('{project}', '{input}', '{output}')) {
    $occurrences = 0
    foreach ($argument in $templates) { $occurrences += [regex]::Matches($argument, [regex]::Escape($placeholder)).Count }
    if ($occurrences -ne 1) { throw "ConsoleArguments must contain exactly one $placeholder placeholder." }
}
$arguments = @(
    foreach ($argument in $templates) {
        $argument.Replace('{project}', $project).Replace('{input}', $input).Replace('{output}', $output)
    }
)

$logDirectory = Split-Path -Parent $compilerLog
if (-not (Test-Path -LiteralPath $logDirectory -PathType Container)) {
    New-Item -ItemType Directory -Path $logDirectory | Out-Null
}
$nonce = [Guid]::NewGuid().ToString('N')
$stdout = Join-Path $logDirectory ('.vmprotect-console-stdout-' + $nonce + '.tmp')
$stderr = Join-Path $logDirectory ('.vmprotect-console-stderr-' + $nonce + '.tmp')
try {
    $process = Start-Process -FilePath $console -ArgumentList $arguments -WorkingDirectory (Split-Path -Parent $project) `
        -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $combined = @()
    if (Test-Path -LiteralPath $stdout) { $combined += Get-Content -LiteralPath $stdout }
    if (Test-Path -LiteralPath $stderr) { $combined += Get-Content -LiteralPath $stderr }
    [IO.File]::WriteAllLines($compilerLog, [string[]]$combined, [Text.UTF8Encoding]::new($false))
    if ($process.ExitCode -ne 0) { throw "VMProtect console failed with exit code $($process.ExitCode)." }
}
finally {
    foreach ($temporary in @($stdout, $stderr)) {
        if (Test-Path -LiteralPath $temporary -PathType Leaf) { Remove-Item -LiteralPath $temporary }
    }
}

if (-not (Test-Path -LiteralPath $output -PathType Leaf) -or (Get-Item -LiteralPath $output).Length -le 0) {
    throw 'VMProtect console returned success without producing the exact protected output.'
}
& (Join-Path $PSScriptRoot 'vmp\accept-manual-output.ps1') -PreparedManifest $preparedPath -MarkerReviewPath $MarkerReviewPath
