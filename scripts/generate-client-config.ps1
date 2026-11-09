param([ValidateSet('json','toml','deepseek','qwenpaw')][string]$Format='json', [string]$OutputPath)
$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$adapter=Join-Path $taskRoot 'putty-ai-mcp.exe'
if(!(Test-Path -LiteralPath $adapter)) { $adapter=Join-Path $taskRoot 'dist/putty-ai-v4.4.0-x64/putty-ai-mcp.exe' }
$adapter=(Resolve-Path -LiteralPath $adapter).Path
if($Format -in 'json','qwenpaw') {
    $entry=[ordered]@{command=$adapter;args=@()}
    if($Format -eq 'qwenpaw') {
        $entry.transport='stdio'
        $entry.env=[ordered]@{
            PUTTY_MCP_SOURCE='QwenPaw'
            PUTTY_MCP_DIAGNOSTICS='qwenpaw'
            LOCALAPPDATA=[Environment]::GetFolderPath('LocalApplicationData')
        }
    }
    $result=[ordered]@{mcpServers=[ordered]@{putty=$entry}} | ConvertTo-Json -Depth 8
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
