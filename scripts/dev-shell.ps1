# Build shell for depth.
#
# MSVC's `link.exe` is not on PATH by default, and `whisper-rs` needs CMake and a C++
# compiler, so Cargo cannot build this project from a plain terminal. This script imports
# the Visual Studio environment and then runs whatever you pass to it (default: a release
# build).
#
#   .\scripts\dev-shell.ps1                       # cargo build --release
#   .\scripts\dev-shell.ps1 cargo test            # run the test suite
#   .\scripts\dev-shell.ps1 cargo run -- --check  # run the app
#
# Requires: Visual Studio Build Tools with the "Desktop development with C++" workload
# (see README.md for the one-line install command).

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $Command
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# 1. Locate the Visual Studio installation that has the C++ tools.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) {
    throw "vswhere.exe not found. Install Visual Studio Build Tools with the C++ workload (see README.md)."
}
$vsPath = & $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath
if (-not $vsPath) {
    throw "No Visual Studio installation with the C++ tools was found. Add the 'Desktop development with C++' workload."
}

# 2. Import the MSVC environment (vcvars64.bat) into this process.
$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) {
    throw "vcvars64.bat not found under $vsPath."
}
Write-Host "Using Visual Studio at $vsPath" -ForegroundColor DarkGray
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        Set-Item -Path "env:$($matches[1])" -Value $matches[2]
    }
}

# 3. Make the workspace-local Rust toolchain and CMake reachable.
$cargoHome = Join-Path $root '.toolchain\cargo'
$env:CARGO_HOME = $cargoHome
$env:RUSTUP_HOME = Join-Path $root '.toolchain\rustup'
$env:Path = "$cargoHome\bin;$env:Path"

# CMake ships with the VS C++ workload but is not added to PATH by vcvars.
$vsCMake = Join-Path $vsPath 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'
if (Test-Path $vsCMake) {
    $env:Path = "$vsCMake;$env:Path"
} elseif (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    Write-Warning "cmake was not found; whisper-rs needs it to build whisper.cpp."
}

# 4. Run the command (default: release build).
if (-not $Command -or $Command.Count -eq 0) {
    $Command = @('cargo', 'build', '--release')
}
Write-Host "> $($Command -join ' ')" -ForegroundColor DarkGray

$exe = $Command[0]
$rest = @()
if ($Command.Count -gt 1) { $rest = $Command[1..($Command.Count - 1)] }
Push-Location $root
try {
    & $exe @rest
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
