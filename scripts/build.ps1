# Builds the Rust core (livelisten_native.dll, live-listen.exe) and the WinUI 3 app.
#   .\scripts\build.ps1            Debug build
#   .\scripts\build.ps1 -Release   Release build
#   .\scripts\build.ps1 -Publish   Release build plus a self-contained folder in dist\LiveListen
param(
    [switch]$Release,
    [switch]$Publish
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$config = if ($Release -or $Publish) { 'Release' } else { 'Debug' }
$project = Join-Path $root 'app\LiveListen\LiveListen.csproj'

Push-Location $root
try {
    $cargoArgs = @('build', '-p', 'live-listen-ffi', '-p', 'live-listen-cli', '--locked')
    if ($config -eq 'Release') { $cargoArgs += '--release' }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }

    if ($Publish) {
        $out = Join-Path $root 'dist\LiveListen'
        & dotnet publish $project -c Release -p:Platform=x64 -o $out
        if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed ($LASTEXITCODE)" }
        Write-Host "Published to $out"
    }
    else {
        & dotnet build $project -c $config -p:Platform=x64
        if ($LASTEXITCODE -ne 0) { throw "dotnet build failed ($LASTEXITCODE)" }
    }
}
finally {
    Pop-Location
}
