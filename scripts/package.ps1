param([string]$PackageName='putty-ai-v4.4.0-x64', [string]$SourcePackageName='putty-ai-v4.4.0-source')
$ErrorActionPreference = 'Stop'
if($PackageName -notmatch '^putty-ai[-.a-zA-Z0-9]*$') { throw 'Invalid package directory name' }
if($SourcePackageName -notmatch '^putty-ai[-.a-zA-Z0-9]*$') { throw 'Invalid source directory name' }
$taskRoot = Split-Path -Parent $PSScriptRoot
& (Join-Path $PSScriptRoot 'third-party.ps1')
$packagePath = Join-Path $taskRoot "dist/$PackageName"
New-Item -ItemType Directory -Force $packagePath | Out-Null
$extra = Get-ChildItem -LiteralPath $packagePath -File -Filter '*.exe' |
    Where-Object Name -NotIn @('putty.exe','putty-ai-mcp.exe')
if ($extra) { throw 'Runtime folder contains other executables; choose a new package name' }
function Copy-RuntimeBinary([string]$SourcePath) {
    $destination=Join-Path $packagePath (Split-Path -Leaf $SourcePath)
    if((Test-Path -LiteralPath $destination) -and
       (Get-FileHash -LiteralPath $SourcePath -Algorithm SHA256).Hash -eq
       (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash) { return }
    Copy-Item -LiteralPath $SourcePath -Destination $destination -Force
}
Copy-RuntimeBinary (Join-Path $taskRoot 'build-ai/Release/putty.exe')
Copy-RuntimeBinary (Join-Path $taskRoot 'putty-ai-rs/target/release/putty-ai-mcp.exe')
Copy-Item -LiteralPath (Join-Path $taskRoot 'putty-0.85/LICENCE') -Destination $packagePath -Force
Copy-Item -LiteralPath (Join-Path $taskRoot 'README-AI.md') -Destination $packagePath -Force
Copy-Item -LiteralPath (Join-Path $taskRoot 'START_HERE.md') -Destination $packagePath -Force
Copy-Item -LiteralPath (Join-Path $taskRoot 'FEED_TO_DEEPSEEK.txt') -Destination $packagePath -Force
Copy-Item -LiteralPath (Join-Path $taskRoot 'docs'),(Join-Path $taskRoot 'examples') -Destination $packagePath -Recurse -Force
New-Item -ItemType Directory -Force (Join-Path $packagePath 'scripts') | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-mcp.ps1') -Destination (Join-Path $packagePath 'scripts') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'check-mcp.ps1') -Destination (Join-Path $packagePath 'scripts') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'generate-client-config.ps1') -Destination (Join-Path $packagePath 'scripts') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'connection-assistant.ps1') -Destination (Join-Path $packagePath 'scripts') -Force
# Shareable template; generate-client-config.ps1 resolves each recipient's own path.
Copy-Item -LiteralPath (Join-Path $taskRoot 'examples/config.toml') -Destination (Join-Path $packagePath 'config.toml') -Force
$hashes = Get-ChildItem -LiteralPath $packagePath -Filter '*.exe' | Get-FileHash -Algorithm SHA256 | Select-Object @{n='file';e={Split-Path -Leaf $_.Path}},Hash
$hashes | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $packagePath 'SHA256.json') -Encoding utf8
Compress-Archive -LiteralPath $packagePath -DestinationPath (Join-Path $taskRoot "dist/$PackageName.zip") -Force
$sourcePackage=Join-Path $taskRoot "dist/$SourcePackageName"
$rustPackage=Join-Path $sourcePackage 'putty-ai-rs'
New-Item -ItemType Directory -Force $rustPackage | Out-Null
foreach ($item in @('putty-0.85','docs','scripts','tests','examples','installer','README-AI.md','START_HERE.md','FEED_TO_DEEPSEEK.txt','putty-0.85.tar.gz')) {
    Copy-Item -LiteralPath (Join-Path $taskRoot $item) -Destination $sourcePackage -Recurse -Force
}
foreach ($item in @('common','broker','mcp','Cargo.toml','Cargo.lock')) {
    Copy-Item -LiteralPath (Join-Path $taskRoot "putty-ai-rs/$item") -Destination $rustPackage -Recurse -Force
}
Compress-Archive -LiteralPath $sourcePackage -DestinationPath (Join-Path $taskRoot "dist/$SourcePackageName.zip") -Force
Write-Output "Packaged: $packagePath"
Write-Output "Source: $sourcePackage"

