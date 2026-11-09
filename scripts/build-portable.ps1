$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$framework=Join-Path $env:WINDIR 'Microsoft.NET/Framework64/v4.0.30319'
$compiler=Join-Path $framework 'csc.exe'
$payload=(Resolve-Path -LiteralPath (Join-Path $taskRoot 'dist/putty-ai-v4.4.0-x64.zip')).Path
$source=Join-Path $taskRoot 'installer/Portable.cs'
$output=Join-Path $taskRoot 'dist/PuTTY-MCP-Portable-4.4.0-x64.exe'
& $compiler /nologo /optimize+ /target:winexe /platform:x64 /reference:System.Windows.Forms.dll `
    ('/reference:'+(Join-Path $framework 'System.IO.Compression.dll')) `
    "/resource:$payload,PuTTY-MCP.payload.zip" "/out:$output" $source
if($LASTEXITCODE) { throw 'Portable executable build failed' }
Write-Output "Portable: $output"
