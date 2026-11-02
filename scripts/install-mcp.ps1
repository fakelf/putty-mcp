param([string]$AdapterPath, [switch]$Register)
$ErrorActionPreference = 'Stop'
if (!$AdapterPath) {
    $taskRoot=Split-Path -Parent $PSScriptRoot
    $AdapterPath=Join-Path $taskRoot 'putty-ai-mcp.exe'
    if (!(Test-Path -LiteralPath $AdapterPath)) { $AdapterPath=Join-Path $taskRoot 'dist/putty-ai-v2-fixed-x64/putty-ai-mcp.exe' }
}
$AdapterPath = (Resolve-Path -LiteralPath $AdapterPath).Path
Write-Output "codex mcp add putty -- `"$AdapterPath`""
if ($Register) {
    $codexCommand=Get-Command codex -ErrorAction SilentlyContinue
    $codexExecutable=if($codexCommand) { $codexCommand.Source } else { $null }
    if(!$codexExecutable) {
        $desktopBin=Join-Path $env:LOCALAPPDATA 'OpenAI/Codex/bin'
        if(Test-Path -LiteralPath $desktopBin) {
            $candidate=Get-ChildItem -LiteralPath $desktopBin -Directory | ForEach-Object {
                $executable=Join-Path $_.FullName 'codex.exe'
                if(Test-Path -LiteralPath $executable) { Get-Item -LiteralPath $executable }
            } | Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if($candidate) { $codexExecutable=$candidate.FullName }
        }
    }
    if(!$codexExecutable) { throw 'Codex CLI not found. Install Codex or configure this stdio server in your other MCP client.' }
    # A first-time `mcp get putty` emits stderr; Windows PowerShell treats it
    # as a terminating error under Stop. List safely and select only this server.
    $existingJson=& $codexExecutable mcp list --json
    if($LASTEXITCODE) { throw 'Cannot inspect the current Codex MCP configuration' }
    $serverDefinitions=ConvertFrom-Json -InputObject ($existingJson -join "`n")
    $existing=@($serverDefinitions) | Where-Object { $_.name -eq 'putty' } | Select-Object -First 1
    if($existing -and $existing.enabled -and $existing.transport.command -eq $AdapterPath) {
        Write-Output 'PuTTY MCP is already registered at this path; preserving its current policy.'
    } else {
        $configRoot=if($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
        $configFile=Join-Path $configRoot 'config.toml'
        if(Test-Path -LiteralPath $configFile) {
            $backup=$configFile+'.before-putty-'+(Get-Date -Format 'yyyyMMdd-HHmmss-fff')+'.bak'
            Copy-Item -LiteralPath $configFile -Destination $backup
            Write-Output "Previous configuration backed up: $backup"
        }
        if($existing -and $existing.enabled -and $existing.transport.type -eq 'stdio' -and (Test-Path -LiteralPath $configFile)) {
            # Upgrade only this executable path; preserve the user's tool approval policy.
            $text=[IO.File]::ReadAllText($configFile)
            $blocks=[regex]::Matches($text,'(?ms)^\[mcp_servers\.putty\][ \t]*\r?\n(?<body>.*?)(?=^\[|\z)')
            if($blocks.Count -ne 1) { throw 'Cannot safely locate the existing PuTTY config table' }
            $block=$blocks[0]
            if([regex]::Matches($block.Value,'(?m)^command\s*=').Count -ne 1) { throw 'Cannot safely locate the PuTTY command field' }
            $commandLine='command = '+(ConvertTo-Json -InputObject $AdapterPath -Compress)
            $replacement=[regex]::Replace($block.Value,'(?m)^command\s*=.*$',{param($match) $commandLine})
            $text=$text.Substring(0,$block.Index)+$replacement+$text.Substring($block.Index+$block.Length)
            [IO.File]::WriteAllText($configFile,$text,[Text.UTF8Encoding]::new($false))
        } else {
            & $codexExecutable mcp add putty -- $AdapterPath
            if ($LASTEXITCODE) { throw 'Codex registration failed' }
        }
    }
    & $codexExecutable mcp get putty --json
    if ($LASTEXITCODE) { throw 'Codex registration verification failed' }
    Write-Output 'Registered. Reload your MCP client before use. Additional per-tool approval examples are in examples/config.toml.'
} else {
    Write-Output 'Preview only. Run with -Register to edit your real Codex configuration. Merge the example per-tool approval policy separately.'
}

