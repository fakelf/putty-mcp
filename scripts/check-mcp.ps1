param([string]$AdapterPath, [string]$SessionId, [switch]$ReadTerminal)
$ErrorActionPreference='Stop'
if($ReadTerminal -and !$SessionId) { throw '-ReadTerminal requires an explicitly selected -SessionId' }
if(!$AdapterPath) {
    $taskRoot=Split-Path -Parent $PSScriptRoot
    $AdapterPath=Join-Path $taskRoot 'putty-ai-mcp.exe'
    if(!(Test-Path -LiteralPath $AdapterPath)) { $AdapterPath=Join-Path $taskRoot 'dist/putty-ai-v2-fixed-x64/putty-ai-mcp.exe' }
}
$AdapterPath=(Resolve-Path -LiteralPath $AdapterPath).Path
$start=[Diagnostics.ProcessStartInfo]::new()
$start.FileName=$AdapterPath
$start.UseShellExecute=$false
$start.CreateNoWindow=$true
$start.RedirectStandardInput=$true
$start.RedirectStandardOutput=$true
$start.RedirectStandardError=$true
$process=[Diagnostics.Process]::new()
$process.StartInfo=$start
$processStarted=$false
$requestNumber=0
function Invoke-McpRequest([string]$Method, $Parameters) {
    $script:requestNumber++
    $id=$script:requestNumber
    $request=@{jsonrpc='2.0';id=$id;method=$Method}
    if($null -ne $Parameters) { $request.params=$Parameters }
    $process.StandardInput.WriteLine(($request | ConvertTo-Json -Depth 20 -Compress))
    $process.StandardInput.Flush()
    $deadline=[DateTime]::UtcNow.AddSeconds(10)
    while([DateTime]::UtcNow -lt $deadline) {
        $lineTask=$process.StandardOutput.ReadLineAsync()
        $remaining=[Math]::Max(1,[int]($deadline-[DateTime]::UtcNow).TotalMilliseconds)
        if(!$lineTask.Wait($remaining)) { throw "MCP response timed out: $Method" }
        $line=$lineTask.Result
        if($null -eq $line) { throw "MCP process closed stdout: $Method" }
        $response=$line | ConvertFrom-Json
        if($response.id -eq $id) {
            if($response.error) { throw ($response.error | ConvertTo-Json -Compress) }
            return $response.result
        }
    }
    throw "MCP response timed out: $Method"
}
function Invoke-McpTool([string]$Name, $Arguments) {
    $result=Invoke-McpRequest 'tools/call' @{name=$Name;arguments=$Arguments}
    if($result.isError) { throw $result.content[0].text }
    return $result.content[0].text | ConvertFrom-Json
}
try {
    if(!$process.Start()) { throw 'Cannot start MCP adapter' }
    $processStarted=$true
    $errorTask=$process.StandardError.ReadToEndAsync()
    $initialized=Invoke-McpRequest 'initialize' @{protocolVersion='2025-03-26';capabilities=@{};clientInfo=@{name='putty-readonly-check';version='1.0'}}
    $process.StandardInput.WriteLine('{"jsonrpc":"2.0","method":"notifications/initialized"}')
    $process.StandardInput.Flush()
    $catalog=Invoke-McpRequest 'tools/list' @{}
    $sessionList=Invoke-McpTool 'list_sessions' @{}
    $report=[ordered]@{
        passed=$true
        adapter=$AdapterPath
        protocol_version=$initialized.protocolVersion
        tools=@($catalog.tools | ForEach-Object name)
        session_count=@($sessionList.sessions).Count
        sessions=$sessionList.sessions
        terminal_content_read=$false
        remote_input_sent=$false
    }
    if($ReadTerminal) {
        if(!@($sessionList.sessions | Where-Object session_id -eq $SessionId).Count) { throw 'Selected session is not currently authorized' }
        $report.terminal=Invoke-McpTool 'read_terminal' @{session_id=$SessionId;cursor=0;max_bytes=4096}
        $report.terminal_content_read=$true
    }
    $report | ConvertTo-Json -Depth 20
} finally {
    if($processStarted) {
        $process.StandardInput.Close()
        if(!$process.WaitForExit(1500)) { $process.Kill(); $process.WaitForExit() }
    }
    $process.Dispose()
}
