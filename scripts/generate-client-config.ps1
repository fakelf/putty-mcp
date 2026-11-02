param([ValidateSet('json','toml','deepseek')][string]$Format='json', [string]$OutputPath)
$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$adapter=Join-Path $taskRoot 'putty-ai-mcp.exe'
if(!(Test-Path -LiteralPath $adapter)) { $adapter=Join-Path $taskRoot 'dist/putty-ai-v2-fixed-x64/putty-ai-mcp.exe' }
$adapter=(Resolve-Path -LiteralPath $adapter).Path
if($Format -eq 'json') {
    $result=[ordered]@{mcpServers=[ordered]@{putty=[ordered]@{command=$adapter;args=@()}}} | ConvertTo-Json -Depth 8
} elseif($Format -eq 'deepseek') {
    $example=Join-Path $taskRoot 'examples/deepseek-harness.cordis.yml'
    $result=[IO.File]::ReadAllText($example).Replace('C:\PuTTY-AI\putty-ai-mcp.exe',$adapter.Replace("'","''"))
} else {
    $example=Join-Path $taskRoot 'examples/config.toml'
    $result=[IO.File]::ReadAllText($example).Replace('C:\PuTTY-AI\putty-ai-mcp.exe',$adapter)
}
if($OutputPath) {
    $resolvedOutput=$ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
    [IO.File]::WriteAllText($resolvedOutput,$result,[Text.UTF8Encoding]::new($false))
    Write-Output "Generated: $resolvedOutput"
} else { Write-Output $result }
