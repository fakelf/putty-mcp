param([switch]$Report)
$ErrorActionPreference='Stop'
$installRoot=Split-Path -Parent $PSScriptRoot
$puttyPath=Join-Path $installRoot 'putty.exe'
$adapterPath=Join-Path $installRoot 'putty-ai-mcp.exe'
$checkPath=Join-Path $PSScriptRoot 'check-mcp.ps1'
$localData=[Environment]::GetFolderPath('LocalApplicationData')
$qwenStatusPath=Join-Path $localData 'PuTTY-AI-v4.4.0/qwenpaw-connection.json'

function Get-LocalInventory {
    $windows=@(Get-Process -Name putty -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $puttyPath }).Count
    [ordered]@{
        putty_present=(Test-Path -LiteralPath $puttyPath -PathType Leaf)
        putty_processes=$windows
        qwenpaw=(Get-QwenPawStatus)
    }
}

function Get-QwenPawStatus {
    if(!(Test-Path -LiteralPath $qwenStatusPath -PathType Leaf)) {
        return [pscustomobject]@{state='not_seen'}
    }
    try {
        if((Get-Item -LiteralPath $qwenStatusPath).Length -gt 16384) { throw 'Unexpected diagnostic file size' }
        $data=ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($qwenStatusPath))
        if($data.adapter -ne $adapterPath) {
            return [pscustomobject]@{state='other_path';adapter=$data.adapter}
        }
        $process=Get-Process -Id ([int]$data.pid) -ErrorAction SilentlyContinue
        $live=$process -and $process.Path -eq $adapterPath -and
            $process.StartTime.ToFileTimeUtc().ToString() -eq $data.process_start
        [pscustomobject]@{state='recorded';live=$live;details=$data}
    } catch {
        [pscustomobject]@{state='unreadable';error=$_.Exception.Message}
    }
}

if($Report) {
    $result=Get-LocalInventory
    $result.local=ConvertFrom-Json -InputObject (& $checkPath -AdapterPath $adapterPath -Detailed)
    $result | ConvertTo-Json -Depth 16
    return
}

Add-Type -AssemblyName System.Windows.Forms,System.Drawing
[Windows.Forms.Application]::EnableVisualStyles()
$form=[Windows.Forms.Form]::new()
$form.Text='PuTTY MCP 连接助手'
$form.StartPosition='CenterScreen'
$form.Size=[Drawing.Size]::new(785,575)
$form.MinimumSize=[Drawing.Size]::new(650,475)
$form.Font=[Drawing.Font]::new('Microsoft YaHei UI',10)
$form.BackColor=[Drawing.Color]::FromArgb(245,247,251)
$form.AutoScaleMode='Dpi'

$layout=[Windows.Forms.TableLayoutPanel]::new()
$layout.Dock='Fill'
$layout.Padding=[Windows.Forms.Padding]::new(20)
$layout.ColumnCount=1
$layout.RowCount=4
$null=$layout.RowStyles.Add([Windows.Forms.RowStyle]::new('AutoSize'))
$null=$layout.RowStyles.Add([Windows.Forms.RowStyle]::new('AutoSize'))
$null=$layout.RowStyles.Add([Windows.Forms.RowStyle]::new('Percent',100))
$null=$layout.RowStyles.Add([Windows.Forms.RowStyle]::new('AutoSize'))
$form.Controls.Add($layout)
$heading=[Windows.Forms.Label]::new()
$heading.Text=@('PuTTY MCP 连接助手','配置 QwenPaw，检查本机链路和宿主连接。') -join [Environment]::NewLine
$heading.AutoSize=$true
$heading.Font=[Drawing.Font]::new('Microsoft YaHei UI',12)
$heading.Margin=[Windows.Forms.Padding]::new(3,4,3,14)
$layout.Controls.Add($heading,0,0)

$buttons=[Windows.Forms.FlowLayoutPanel]::new()
$buttons.Dock='Fill'
$buttons.AutoSize=$true
$buttons.Margin=[Windows.Forms.Padding]::new(0,0,0,12)
$layout.Controls.Add($buttons,0,1)
$open=[Windows.Forms.Button]::new()
$open.Text='打开 PuTTY'
$copy=[Windows.Forms.Button]::new()
$copy.Text='复制 QwenPaw 配置'
$check=[Windows.Forms.Button]::new()
$check.Text='连接自检'
foreach($button in @($open,$copy,$check)) {
    $button.AutoSize=$true
    $button.Padding=[Windows.Forms.Padding]::new(9,4,9,4)
    $buttons.Controls.Add($button)
}

$output=[Windows.Forms.TextBox]::new()
$output.Dock='Fill'
$output.Multiline=$true
$output.ScrollBars='Vertical'
$output.ReadOnly=$true
$output.BackColor=[Drawing.Color]::White
$output.Text=@(
    '打开 PuTTY，登录服务器后从标题栏开启 AI 观察。',
    '',
    '在 QwenPaw 的「智能体 → MCP」粘贴配置并启用。',
    '让 QwenPaw 调用一次 list_sessions，再点「连接自检」。'
) -join [Environment]::NewLine
$layout.Controls.Add($output,0,2)
$footer=[Windows.Forms.Label]::new()
$footer.AutoSize=$true
$footer.Margin=[Windows.Forms.Padding]::new(3,12,3,2)
$footer.Text='自检只查询连接和会话编号。'
$layout.Controls.Add($footer,0,3)

function Show-ConnectionReport($report) {
    $inventory=Get-LocalInventory
    $lines=[Collections.Generic.List[string]]::new()
    $puttyState=if($inventory.putty_present) { '通过' } else { '失败，安装目录缺少 putty.exe' }
    $lines.Add("PuTTY 程序  $puttyState")
    $labels=[ordered]@{
        adapter_path='MCP 程序位置'
        spawn='MCP 进程启动'
        initialize='MCP 握手'
        broker='Broker 后台'
        tools='MCP 工具发现'
        sessions='PuTTY 授权会话'
    }
    foreach($stage in $labels.Keys) {
        $display=switch($report.steps.$stage) {
            'ok' { '通过' }
            'received' { '已到达 MCP，后台连接失败' }
            'empty' { '未发现授权会话' }
            'failed' { '失败' }
            default { '未检查' }
        }
        $lines.Add("$($labels[$stage])  $display")
    }
    if($report.failure) {
        $lines.Add("错误  $($report.failure.message)")
    } elseif($report.session_count -gt 0) {
        $ids=@($report.sessions | ForEach-Object { $_.session_id.Substring(0,8) })
        $lines.Add("窗口编号  $($ids -join '、')")
    } elseif($report.passed) {
        $lines.Add("本安装目录运行的 PuTTY 进程数  $($inventory.putty_processes)")
        $lines.Add('请使用「打开 PuTTY」，登录 SSH 后从标题栏开启 AI 观察。')
    }

    $lines.Add('')
    $lines.Add('QwenPaw 实际连接')
    switch($inventory.qwenpaw.state) {
        'not_seen' {
            $lines.Add('未收到 QwenPaw 启动记录。请把配置导入当前智能体并启用。')
            $lines.Add('若已启用仍无记录，请查 QwenPaw 启动错误，并核对后端电脑和 Windows 用户。')
        }
        'other_path' {
            $lines.Add("最近记录来自其他程序路径  $($inventory.qwenpaw.adapter)")
            $lines.Add('请重新复制本安装目录的配置。')
        }
        'unreadable' { $lines.Add("读取本机记录失败  $($inventory.qwenpaw.error)") }
        'recorded' {
            $record=$inventory.qwenpaw.details
            $time=[DateTimeOffset]::FromUnixTimeMilliseconds([long]$record.updated_ms).LocalDateTime
            $suffix=if($inventory.qwenpaw.live) { '记录对应进程仍在运行' } else { '历史记录，进程已退出' }
            $lines.Add("最近记录  $($time.ToString('yyyy-MM-dd HH:mm:ss'))  $suffix")
            if($record.failure) {
                $stageName=switch($record.failure.stage) {
                    'broker' { '连接 Broker' }
                    'initialize' { 'MCP 握手' }
                    'list_sessions' { '查询 PuTTY 会话' }
                    default { '连接' }
                }
                $lines.Add("$stageName 失败  $($record.failure.message)")
            } elseif(!$record.initialized) {
                $lines.Add('QwenPaw 已启动 MCP 进程，尚未完成握手。')
            } elseif(!$record.tools_listed) {
                $lines.Add('MCP 握手已完成，尚未收到工具清单请求。')
            } elseif($null -eq $record.session_count) {
                $lines.Add('MCP 握手和工具发现已完成。请让 QwenPaw 调用 list_sessions。')
            } else {
                $lines.Add("MCP 握手、工具发现和会话查询已完成。QwenPaw 查到 $($record.session_count) 个会话。")
            }
        }
    }
    $output.Text=$lines -join [Environment]::NewLine
    $output.SelectionStart=0
}

$open.Add_Click({
    try {
        Start-Process -FilePath $puttyPath -WorkingDirectory $installRoot
        $footer.Text='PuTTY 已打开，登录后开启 AI 观察。'
    } catch { $footer.Text=$_.Exception.Message }
})
$copy.Add_Click({
    try {
        $json=& (Join-Path $PSScriptRoot 'generate-client-config.ps1') -Format qwenpaw
        [Windows.Forms.Clipboard]::SetText($json)
        $footer.Text='已复制。请粘贴到 QwenPaw 的「智能体 → MCP」。'
    } catch { $footer.Text=$_.Exception.Message }
})
$script:runner=$null
$script:pending=$null
$timer=[Windows.Forms.Timer]::new()
$timer.Interval=150
$check.Add_Click({
    $check.Enabled=$false
    $footer.Text='正在检查本机连接及 QwenPaw 记录…'
    try {
        $script:runner=[PowerShell]::Create()
        $null=$script:runner.AddCommand($checkPath).AddParameter('AdapterPath',$adapterPath).AddParameter('Detailed')
        $script:pending=$script:runner.BeginInvoke()
        $timer.Start()
    } catch {
        if($script:runner) { $script:runner.Dispose(); $script:runner=$null }
        $check.Enabled=$true
        $footer.Text=$_.Exception.Message
    }
})
$timer.Add_Tick({
    if(!$script:pending.IsCompleted) { return }
    $timer.Stop()
    try {
        $result=$script:runner.EndInvoke($script:pending) -join [Environment]::NewLine
        Show-ConnectionReport (ConvertFrom-Json -InputObject $result)
        $footer.Text='自检完成。可选中文本复制给技术支持。'
    } catch {
        $footer.Text=$_.Exception.Message
    } finally {
        $script:runner.Dispose()
        $script:runner=$null
        $script:pending=$null
        $check.Enabled=$true
    }
})
try {
    $null=$form.ShowDialog()
} finally {
    $timer.Stop()
    if($script:runner) { $script:runner.Dispose() }
    $timer.Dispose()
    $form.Dispose()
}
