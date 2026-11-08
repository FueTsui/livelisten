# Build the installer, portable ZIP and checksums without uploading artifacts.
param(
    [string]$PublishDirectory,
    [string]$OutputDirectory,
    [string]$CoreDirectory,
    [string]$Revision = '',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$packagePython = (Get-Command python -ErrorAction Stop).Source
if (!$OutputDirectory) { $OutputDirectory = Join-Path $projectRoot ('target\release-check\package-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
if (!$PublishDirectory) { $PublishDirectory = Join-Path $projectRoot ('target\release-check\publish-' + [Guid]::NewGuid().ToString('N')) }
$PublishDirectory = [System.IO.Path]::GetFullPath($PublishDirectory)
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must be new; existing artifacts are never overwritten.' }
if (!$SkipBuild) {
    if (Test-Path -LiteralPath $PublishDirectory) { throw 'PublishDirectory must be new when building.' }
    Push-Location $projectRoot
    $previousRustFlags = [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process')
    try {
        $staticTarget = Join-Path $projectRoot ('target\release-check\rust-static-' + [Guid]::NewGuid().ToString('N'))
        $env:CARGO_ENCODED_RUSTFLAGS = '-C' + [char]31 + 'target-feature=+crt-static'
        & cargo build --locked --release --target-dir $staticTarget -p live-listen-ffi -p live-listen-cli -p live-listen-setup
        if ($LASTEXITCODE -ne 0) { throw 'Rust build failed.' }
        $CoreDirectory = Join-Path $staticTarget 'x86_64-pc-windows-msvc\release'
        & dotnet publish 'app\LiveListen\LiveListen.csproj' -c Release -r win-x64 --self-contained true -p:Platform=x64 -p:WindowsAppSDKSelfContained=true -o $PublishDirectory
        if ($LASTEXITCODE -ne 0) { throw '.NET self-contained publish failed.' }
    }
    finally {
        [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', $previousRustFlags, 'Process')
        Pop-Location
    }
}
if (!(Test-Path -LiteralPath $PublishDirectory -PathType Container)) { throw 'PublishDirectory is missing.' }
if (!$CoreDirectory -or !(Test-Path -LiteralPath $CoreDirectory -PathType Container)) { throw 'CoreDirectory must contain the static CRT core, CLI, setup.exe and pack.exe when using SkipBuild.' }
$packageArguments = @((Join-Path $PSScriptRoot 'package_portable.py'), '--publish-dir', $PublishDirectory, '--output-dir', $OutputDirectory, '--core-dir', ([IO.Path]::GetFullPath($CoreDirectory)))
if ($Revision) { $packageArguments += @('--revision', $Revision) }
& $packagePython @packageArguments
if ($LASTEXITCODE -ne 0) { throw 'Package validation failed.' }
