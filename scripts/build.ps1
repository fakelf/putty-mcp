param([switch]$RunTests)
$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$sourcePath = Join-Path $taskRoot 'putty-0.85'
$buildPath = Join-Path $taskRoot 'build-ai'
$rustPath = Join-Path $taskRoot 'putty-ai-rs'
& cmake -S $sourcePath -B $buildPath -G 'Visual Studio 17 2022' -A x64 -DPUTTY_AI_BRIDGE=ON
if ($LASTEXITCODE) { throw 'CMake configure failed' }
& cmake --build $buildPath --config Release --target putty test_ai_ring test_ai_native test_ai_command -j 8
if ($LASTEXITCODE) { throw 'Native build failed' }
Push-Location $rustPath
try {
    & cargo build --workspace --release --locked
    if ($LASTEXITCODE) { throw 'Rust build failed' }
} finally { Pop-Location }
if ($RunTests) {
    & (Join-Path $PSScriptRoot 'test.ps1')
}
& (Join-Path $PSScriptRoot 'package.ps1')

