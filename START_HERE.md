# PuTTY MCP v4.4.0

使用 MSI 安装时，从开始菜单打开「PuTTY MCP 连接助手」。它可以打开同包 PuTTY、复制 QwenPaw 配置，以及检查本机链路和 QwenPaw 实际连接。详见 [QwenPaw 接入和排查](docs/qwenpaw.md)。

使用免安装包时，解压 `putty-ai-v4.4.0-x64.zip`，让 `putty.exe` 和 `putty-ai-mcp.exe` 保持同目录。适用于 Windows x64，需要 VC++ 2015–2022 x64 Runtime。AI 宿主和 PuTTY 需要在同一台 Windows 电脑、同一个用户下运行。

## 连接和自检

1. 让 AI 宿主启动本包的 `putty-ai-mcp.exe`，配置为本地 stdio，参数留空。后台调度由同一个程序自动启动。
2. 用本包的 `putty.exe` 手动登录 SSH，到目标 Shell 后在标题栏左上角菜单选择 **Enable AI...**。开启后同一项变为 **Disable AI**。
3. 两步顺序可以互换。先开 PuTTY 时，MCP 启动前可能显示 `AI OFFLINE`，稍后会自动接上。先开 MCP 时，会话列表在开启观察前可能为空。
4. 让 AI 调用 `list_sessions`，核对列表中的完整会话 ID 和标题显示的前八位。v4.4.0 使用独立通道，升级时 PuTTY 和 MCP 宿主配置均需切换到新包。

排查连接问题可使用独立的「PuTTY MCP 连接助手」，也可在解压目录运行 `.\putty-ai-mcp.exe --connection-report off --text` 查看 Broker、MCP 宿主及最近一次 QwenPaw 连接。该报告不启动 Broker。

在解压目录运行自检。它检查十二个工具和会话列表。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-mcp.ps1
```

## Codex

在解压目录运行以下命令注册或切换到本包。重启 MCP 客户端或重新开启会话，让工具清单刷新。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-mcp.ps1 -Register
```

已有自定义工具白名单时，按需加入 `attach_terminal`、`read_task_screen`、`request_task_action` 和要使用的其他工具。完整配置示例可由 `generate-client-config.ps1 -Format toml` 生成。模板中的 `PUTTY_MCP_SOURCE` 会在 PuTTY 审批中标记来源为 Codex。

## DeepSeek Harness

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\generate-client-config.ps1 -Format deepseek -OutputPath .\putty.cordis.yml
dsh web --patch "$PWD\putty.cordis.yml"
```

给 AI 读 `FEED_TO_DEEPSEEK.txt`。完整说明见 [DeepSeek Harness 接入](docs/deepseek-harness.md)。

## 命令和中断

AI 调用 `submit_command` 后立即得到 `task_id`。普通模式下 PuTTY 显示来源、目标和命令正文，用户逐条批准。多窗口自动模式在校验后直接发送。PuTTY 只发送命令正文和 Enter。AI 用 `get_task_status` 读取设备回显，并通过提示符判断完成。提示符会变或无法自动识别时，可传已知的 `expected_prompt`。命令任务的 `exit_code` 为 `null`。

例如，H3C 设备显示 `<Device>` 时，让 AI 只提交 `display current-configuration`。设备自己的回显和报错照常显示。若出现分页，AI 可在同一任务上读取最新画面，再用 `press_key` 发送一次 `space` 翻页。[H3C CLI 手册](https://www.h3c.com/en/d_202605/2842234_294551_0.htm)

- 未发出的命令可用 `cancel_task` 请求撤销。
- 当前已运行的任务可用 `interrupt_task` 请求发送一次 Ctrl+C。程序检查任务状态后向前台终端送出按键，普通模式会先弹窗。查询结果中的 `interrupt_status: sent` 表示按键已交给 PuTTY，任务结果另外看。
- `get_task_status` 默认返回清理后的命令输出。需要排查原始回显时，使用 `output_view: transcript` 或 `read_terminal`。
- 让 AI 使用 `start_interactive` 启动 `top` 或空缓冲区的 `vi`，用 `read_task_screen` 读画面。交互任务在主 PuTTY 窗口显示，退出后自动恢复 Shell。使用 `request_task_action` 发送命名按键或向 vi 插入文本。退出用 `quit`，明确同意丢弃 vi 修改时用 `discard_and_quit`，中断用 `interrupt_task`。适用于 `list_sessions` 标有对应交互能力的直连 SSH-2 主窗口。
- 编辑服务器上的已有文件时，先核对会话有 `interactive_vi_file` 能力，再让 AI 用 `start_interactive` 传入 `program: vi` 和绝对 `path`。编辑完成后，用 `request_task_action` 的 `save` 保存。普通模式的打开和保存审批会显示文件路径。保存后核对 vi 回显和实际文件内容。

接口细节见 [AI 接入手册](docs/ai-integration.md) 和 [工具 Schema](docs/mcp-tools.json)。

## 前台终端键盘

要操作已经打开的 Shell、设备 CLI 或交互程序，让 AI 调用 `attach_terminal` 附加到正确的 PuTTY 主窗口或子窗口。如果命令已由 MCP 提交，直接用它的 `task_id`。先用 `read_task_screen` 看当前画面，再通过 `request_task_action` 发送单键或组合键。`type_text` 输入至多 1024 个 UTF-8 字节，无需附加 Enter。`paste_text` 需要程序支持括号粘贴。

依赖画面的按键可带上 `if_frame`，PuTTY 在实际发送时核对画面版本。任务超时或结果未知时，若仍持有前台终端，继续输入需要最新 `if_frame`。结束后可 `detach`。你也可以直接在 PuTTY 输入并接管，SSH 连接保留。普通模式在 PuTTY 确认，自动模式按窗口已有的授权执行。

## 多子终端

PuTTY 默认处于普通模式。要启用多子终端，先开启 AI，再从左上角菜单勾选 **AI multi-window mode (auto-run)**。确认主窗口标题显示 **AUTO RUN**，`list_sessions` 显示 `approval_mode: automatic`。这时主窗口和 AI 子终端执行各自支持的操作，无需在 PuTTY 逐条点击。AI 宿主可以继续显示它自己的确认。

用 `open_child_session` 打开子终端，再用 `get_task_status` 和 `list_sessions` 确认。子终端与主窗口共用已认证的 SSH 连接，各自有独立的黑色窗口和会话 ID，每个主窗口最多四个。PuTTY 优先为新窗口寻找重叠较少的位置，已有窗口保持原位。用 `close_child_session` 关闭空闲的子终端。

切回普通模式会恢复主窗口的逐条批准，并撤销子终端的 AI 权限。子窗口和正在运行的命令可继续手动操作。关闭主窗口的 SSH 连接会结束其子终端。
