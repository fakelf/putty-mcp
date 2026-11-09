param([Parameter(Mandatory)][string]$WixPath)
$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$compiler=(Resolve-Path -LiteralPath $WixPath).Path
$packageName='putty-ai-v4.4.0-x64'
$payload=Join-Path $taskRoot "dist/$packageName"
$installer=Join-Path $taskRoot 'dist/PuTTY-MCP-Setup-4.4.0-x64.msi'
$bundle=Join-Path $taskRoot 'dist/PuTTY-MCP-Setup-4.4.0-x64.exe'
& (Join-Path $PSScriptRoot 'package.ps1')
& $compiler build -arch x64 -d "Payload=$payload" -o $installer (Join-Path $taskRoot 'installer/Package.wxs')
if($LASTEXITCODE) { throw 'WiX installer build failed' }
Push-Location (Split-Path -Parent $compiler)
try {
    $bal='WixToolset.Bal.wixext/4.0.6'
    & $compiler extension add $bal
    if($LASTEXITCODE) { throw 'WiX bootstrapper extension could not be restored' }
    & $compiler build -arch x64 -ext $bal -d "MsiPath=$installer" -o $bundle (Join-Path $taskRoot 'installer/Bundle.wxs')
    if($LASTEXITCODE) { throw 'WiX EXE installer build failed' }
} finally { Pop-Location }
Write-Output "Installer: $bundle"
