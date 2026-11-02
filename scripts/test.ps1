$ErrorActionPreference = 'Stop'
$taskRoot = Split-Path -Parent $PSScriptRoot
$reports = Join-Path $taskRoot 'docs'
Push-Location (Join-Path $taskRoot 'putty-ai-rs')
try {
    & cargo fmt --all --check
    if ($LASTEXITCODE) { throw 'Rust formatting failed' }
    & cargo clippy --workspace --all-targets --locked -- -D warnings
    if ($LASTEXITCODE) { throw 'Clippy failed' }
    & cargo test --workspace --locked -- --nocapture 2>&1 | Tee-Object (Join-Path $reports 'rust-tests.txt')
    if ($LASTEXITCODE) { throw 'Rust tests failed. Stop any existing broker before isolated tests.' }
} finally { Pop-Location }
& (Join-Path $taskRoot 'build-ai/Release/test_ai_ring.exe') | Tee-Object (Join-Path $reports 'native-ring-results.json')
if ($LASTEXITCODE) { throw 'Native ring test failed' }
& node (Join-Path $taskRoot 'tests/mcp-smoke.mjs')
if ($LASTEXITCODE) { throw 'MCP/native integration tests failed' }
& (Join-Path $taskRoot 'build-ai/Release/test_ai_command.exe')
if ($LASTEXITCODE) { throw 'Native command validation failed' }
& node (Join-Path $taskRoot 'tests/shell-quoting.mjs')
if ($LASTEXITCODE) { throw 'POSIX shell quoting tests failed (requires Git Bash or PUTTY_TEST_BASH)' }

