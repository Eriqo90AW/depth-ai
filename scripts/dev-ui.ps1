# Starts the isolated Slint development app with live layout updates.
param([ValidateSet('empty','recording','long','settings','error')][string] $Preview, [switch] $Software)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$env:SLINT_LIVE_PREVIEW = '1'
if ($Software) { $env:SLINT_BACKEND = 'winit-software' }
$buildTemp = Join-Path $root '.scratch\build-temp'
New-Item -ItemType Directory -Force $buildTemp | Out-Null
$env:TEMP = $buildTemp
$env:TMP = $buildTemp
# Keep the dev executable separate from the regular app binary. Cargo places
# the explicitly suffixed rustc output in deps, avoiding an installed/running depth.exe.
& (Join-Path $PSScriptRoot 'dev-shell.ps1') cmd /c 'cargo rustc --bin depth --features slint/live-preview -- -C extra-filename=-live'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
# Sherpa's runtime DLLs are copied to target/debug. The dev executable is in
# deps, so Windows needs the parent directory on this process's DLL search path.
$runtimeDir = Join-Path $root 'target\debug'
$env:PATH = "$runtimeDir;$env:PATH"
$arguments = @('--home', '.scratch/dev')
if ($Preview) { $arguments += @('--preview-state', $Preview) }
& (Join-Path $root 'target\debug\deps\depth-live.exe') @arguments
exit $LASTEXITCODE
